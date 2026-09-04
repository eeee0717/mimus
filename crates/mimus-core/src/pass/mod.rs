use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use lopdf::{Document as LopdfDocument, Object, ObjectId, xref::XrefEntry};
use rayon::prelude::*;

use crate::context::{
    CharacterAlignment, Document, ExtractedPage, OutputFont, OutputFonts, PassContext,
};
use crate::engine::{PageCharSnapshot, RgbaImage};
use crate::error::{
    AssetReason, ErrorReason, InputErrorDetail, InputReason, InternalReason, IoReason, MimusError,
    Result, TranslationReason, UsageReason,
};
use crate::event::{
    Diagnostic, Diagnostics, Event, EventKind, FormulaBoundaryEvidence,
    MAX_REPORTED_FORM_OBJECT_IDS, PageDegradeReason, PreservedParagraph, RecoveryKind, Stage,
    SuspiciousEchoParagraph,
};
use crate::geometry::{PageFrame, PageGeometryResolveError};
use crate::il::{
    self, Char, LayoutAssignment, LayoutLabel, LayoutSource, PageGeometry, Paragraph,
    PassthroughRef, Point, Rect, TextCarrier, TextTransform, TranslationPolicy,
};
use crate::scan::{PageClass, prescan_page};
#[cfg(test)]
use crate::walk::walk_page;
use crate::walk::{PageWalkError, UnicodeProvenance, walk_page_detailed_with_rotation};
#[cfg(test)]
use crate::write::build_incremental;
use crate::write::{
    ContentSpanReplacement, EmbeddedFont, PageRewrite, TypesetCharacter, WriteOptions,
    build_incremental_with_options, glyph_width_1000, publish,
};

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
const MAX_PAGE_TREE_DEPTH: usize = 128;
const MAX_OBJECT_STREAM_BYTES: usize = 64 * 1024 * 1024;
const FINAL_INK_GEOMETRY_TOLERANCE_PT: f64 = 0.01;

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
        pages: document.il.pages.len() * if context.config.bilingual { 2 } else { 1 },
        warnings: document.diagnostics.warning_count(),
        appended_bytes: write_report.appended_bytes,
    })
}

pub fn inspect(document: &mut Document, context: &PassContext<'_>) -> Result<InspectionResult> {
    run_stages(document, context, &PIPELINE[..INSPECT_STAGE_COUNT])?;
    let il = il::snapshot(&document.il);
    Ok(InspectionResult {
        pages: il.pages.len(),
        warnings: document.diagnostics.warning_count(),
        il,
    })
}

fn run_stages(
    document: &mut Document,
    context: &PassContext<'_>,
    stages: &[(Stage, Pass)],
) -> Result<()> {
    let includes_write = stages.iter().any(|(stage, _)| *stage == Stage::Write);
    for (pass_index, &(stage, pass)) in stages.iter().enumerate() {
        if context.config.strict && stage == Stage::Write && document_has_degradation(document) {
            push_degradation_summary(document);
            return Err(strict_degradation_error(document));
        }
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
    if context.config.strict && !includes_write && document_has_degradation(document) {
        return Err(strict_degradation_error(document));
    }
    Ok(())
}

fn document_has_degradation(document: &Document) -> bool {
    document
        .extracted_pages
        .iter()
        .any(|page| page.degraded.is_some())
        || document
            .il
            .pages
            .iter()
            .flat_map(|page| &page.paragraphs)
            .any(|paragraph| paragraph.preserved.is_some())
}

fn strict_degradation_error(document: &Document) -> MimusError {
    let degraded_pages = document
        .extracted_pages
        .iter()
        .filter(|page| page.degraded.is_some())
        .count();
    let preserved_paragraphs = document
        .il
        .pages
        .iter()
        .flat_map(|page| &page.paragraphs)
        .filter(|paragraph| paragraph.preserved.is_some())
        .count();
    let placeholder_violations = document
        .placeholder_violations
        .iter()
        .map(|(&(page_index, paragraph_index), &violation)| {
            format!(
                "page {} paragraph {} {}",
                page_index + 1,
                paragraph_index + 1,
                violation.wire_name()
            )
        })
        .collect::<Vec<_>>();
    let placeholder_detail = if placeholder_violations.is_empty() {
        String::new()
    } else {
        format!(
            "; placeholder violations: {}",
            placeholder_violations.join(", ")
        )
    };
    MimusError::translation(
        TranslationReason::StrictDegradation,
        format!(
            "strict mode rejected {degraded_pages} degraded pages and {preserved_paragraphs} preserved paragraphs{placeholder_detail}"
        ),
    )
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
    let placeholder_violations = &document.placeholder_violations;
    let suspicious_echoes = document
        .suspicious_echoes
        .iter()
        .map(|&(page_index, paragraph_index)| SuspiciousEchoParagraph {
            page_index,
            paragraph_index,
        })
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
                        placeholder_violation: placeholder_violations
                            .get(&(page.index, paragraph_index))
                            .copied(),
                    })
                })
        })
        .collect::<Vec<_>>();
    if degraded_page_indices.is_empty()
        && preserved_paragraphs.is_empty()
        && suspicious_echoes.is_empty()
    {
        return;
    }
    let suspicious_echo_count = suspicious_echoes.len();
    document.diagnostics.push(Diagnostic::DegradationSummary {
        degraded_pages: degraded_page_indices.len(),
        degraded_page_indices,
        preserved_paragraph_count: preserved_paragraphs.len(),
        preserved_paragraphs,
        suspicious_echoes,
        suspicious_echo_count,
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
    validate_xref_objects(&pdf)?;
    validate_object_streams(&pdf)?;
    let lopdf_pages = validated_page_ids(&pdf)?;
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
        let (geometry, frame, degraded) = match PageFrame::resolve(&pdf, page_id) {
            Ok(frame) => {
                let geometry = frame.geometry();
                let engine_geometry = context.engine.page_geometry(&bytes, index)?;
                validate_input_geometry(index, geometry, engine_geometry)?;
                (geometry, Some(frame), None)
            }
            Err(PageGeometryResolveError::Degraded { reason, .. }) => {
                let geometry =
                    context
                        .engine
                        .page_geometry(&bytes, index)
                        .unwrap_or(PageGeometry {
                            width: 0.0,
                            height: 0.0,
                            rotate_degrees: 0,
                        });
                (geometry, None, Some(reason))
            }
            Err(PageGeometryResolveError::Fatal(error)) => return Err(error),
        };
        let evidence = prescan_page(&pdf, page_id);
        let mut extracted = ExtractedPage {
            index,
            page_id,
            geometry,
            frame,
            evidence,
            class: None,
            degraded: None,
            recoveries: BTreeSet::new(),
            walked_characters: Vec::new(),
            vector_paths: Vec::new(),
            path_ink: Vec::new(),
            inline_images: Vec::new(),
            content_streams: Vec::new(),
            engine_characters: Vec::new(),
            character_alignment: CharacterAlignment::default(),
            layout_regions: Vec::new(),
            input_raster: None,
        };
        if let Some(reason) = degraded {
            degrade_page(&mut extracted, &mut document.diagnostics, reason);
        }
        extracted_pages.push(extracted);
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

fn validate_xref_objects(pdf: &LopdfDocument) -> Result<()> {
    for (&object_number, entry) in &pdf.reference_table.entries {
        let XrefEntry::Normal { offset, generation } = *entry else {
            continue;
        };
        if !pdf.objects.contains_key(&(object_number, generation)) {
            return Err(MimusError::input(
                InputReason::PdfParse,
                format!(
                    "could not parse object {object_number} {generation} at byte offset {offset}"
                ),
            )
            .with_input_detail(InputErrorDetail::ObjectSyntax {
                objid: [object_number, u32::from(generation)],
                offset: u64::from(offset),
            }));
        }
    }
    Ok(())
}

fn validate_object_streams(pdf: &LopdfDocument) -> Result<()> {
    for (&object_id, object) in &pdf.objects {
        let Object::Stream(stream) = object else {
            continue;
        };
        if stream.dict.get(b"Type").and_then(Object::as_name).ok() != Some(b"ObjStm") {
            continue;
        }
        let count = stream
            .dict
            .get(b"N")
            .and_then(Object::as_i64)
            .ok()
            .and_then(|value| usize::try_from(value).ok());
        let first = stream
            .dict
            .get(b"First")
            .and_then(Object::as_i64)
            .ok()
            .and_then(|value| usize::try_from(value).ok());
        let decoded = crate::pdf_stream::decode(pdf, stream, MAX_OBJECT_STREAM_BYTES).ok();
        let valid =
            count
                .zip(first)
                .zip(decoded.as_deref())
                .is_some_and(|((count, first), decoded)| {
                    let Some(header) = decoded.get(..first) else {
                        return false;
                    };
                    let integers = header
                        .split(u8::is_ascii_whitespace)
                        .filter(|token| !token.is_empty())
                        .map(|token| {
                            std::str::from_utf8(token)
                                .ok()
                                .and_then(|token| token.parse::<usize>().ok())
                        })
                        .collect::<Option<Vec<_>>>();
                    let Some(integers) = integers else {
                        return false;
                    };
                    if integers.len() != count.saturating_mul(2) {
                        return false;
                    }
                    let body_len = decoded.len() - first;
                    let mut object_numbers = BTreeSet::new();
                    let mut previous_offset = None;
                    integers.as_chunks::<2>().0.iter().all(|pair| {
                        let object_number = pair[0];
                        let offset = pair[1];
                        let ordered = previous_offset.is_none_or(|previous| offset >= previous);
                        previous_offset = Some(offset);
                        object_number > 0
                            && object_numbers.insert(object_number)
                            && offset <= body_len
                            && ordered
                    })
                });
        if !valid {
            return Err(MimusError::input(
                InputReason::PdfParse,
                format!(
                    "object stream {} {} R has an invalid header",
                    object_id.0, object_id.1
                ),
            ));
        }
    }
    Ok(())
}

fn validated_page_ids(pdf: &LopdfDocument) -> Result<Vec<ObjectId>> {
    let pages_id = pdf
        .catalog()
        .and_then(|catalog| catalog.get(b"Pages"))
        .and_then(Object::as_reference)
        .map_err(|error| {
            MimusError::input(
                InputReason::PdfParse,
                format!("document catalog has no valid Pages reference: {error}"),
            )
        })?;
    let mut pages = Vec::new();
    visit_page_tree(
        pdf,
        pages_id,
        0,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        &mut pages,
    )?;
    Ok(pages)
}

fn visit_page_tree(
    pdf: &LopdfDocument,
    object_id: ObjectId,
    depth: usize,
    active: &mut BTreeSet<ObjectId>,
    visited: &mut BTreeSet<ObjectId>,
    pages: &mut Vec<ObjectId>,
) -> Result<()> {
    if depth > MAX_PAGE_TREE_DEPTH {
        return Err(MimusError::input(
            InputReason::PdfParse,
            format!("page tree exceeds {MAX_PAGE_TREE_DEPTH} inherited levels"),
        ));
    }
    if !active.insert(object_id) {
        return Err(MimusError::input(
            InputReason::PdfParse,
            format!("page tree cycle reaches object {}", object_id.0),
        ));
    }
    if !visited.insert(object_id) {
        active.remove(&object_id);
        return Err(MimusError::input(
            InputReason::PdfParse,
            format!(
                "page tree object {} is referenced more than once",
                object_id.0
            ),
        ));
    }

    let result = (|| {
        let dictionary = pdf.get_dictionary(object_id).map_err(|error| {
            MimusError::input(
                InputReason::PdfParse,
                format!(
                    "page tree object {} is missing or invalid: {error}",
                    object_id.0
                ),
            )
        })?;
        match dictionary.get_type() {
            Ok(b"Page") => pages.push(object_id),
            Ok(b"Pages") => {
                let kids = dictionary
                    .get_deref(b"Kids", pdf)
                    .and_then(Object::as_array)
                    .map_err(|error| {
                        MimusError::input(
                            InputReason::PdfParse,
                            format!("page tree object {} has invalid Kids: {error}", object_id.0),
                        )
                    })?;
                for kid in kids {
                    let kid_id = kid.as_reference().map_err(|error| {
                        MimusError::input(
                            InputReason::PdfParse,
                            format!(
                                "page tree object {} has a non-reference Kid: {error}",
                                object_id.0
                            ),
                        )
                    })?;
                    visit_page_tree(pdf, kid_id, depth + 1, active, visited, pages)?;
                }
            }
            Ok(other) => {
                return Err(MimusError::input(
                    InputReason::PdfParse,
                    format!(
                        "page tree object {} has unsupported Type /{}",
                        object_id.0,
                        String::from_utf8_lossy(other)
                    ),
                ));
            }
            Err(error) => {
                return Err(MimusError::input(
                    InputReason::PdfParse,
                    format!(
                        "page tree object {} has no valid Type: {error}",
                        object_id.0
                    ),
                ));
            }
        }
        Ok(())
    })();
    active.remove(&object_id);
    result
}

fn validate_input_geometry(
    page_index: usize,
    expected: PageGeometry,
    engine: PageGeometry,
) -> Result<()> {
    if expected.rotate_degrees != engine.rotate_degrees
        || !finite_close(expected.width, engine.width, 0.001)
        || !finite_close(expected.height, engine.height, 0.001)
    {
        return Err(MimusError::input(
            InputReason::EngineMismatch,
            format!(
                "page {} geometry differs between the PDF object tree and the inspection engine",
                page_index + 1
            ),
        ));
    }
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
        let frame = page.frame.ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                "a translatable page has no resolved geometry frame",
            )
        })?;
        match walk_page_detailed_with_rotation(pdf, page.page_id, frame.rotate_degrees) {
            Ok(walked) => {
                page.recoveries = walked.recoveries;
                for &recovery in &page.recoveries {
                    let form_cycle_paths = walked
                        .form_cycles
                        .iter()
                        .filter(|path| {
                            let is_self_cycle =
                                path.len() >= 2 && path[path.len() - 1] == path[path.len() - 2];
                            matches!(recovery, RecoveryKind::SelfRecursiveForm) && is_self_cycle
                                || matches!(recovery, RecoveryKind::MutuallyRecursiveForm)
                                    && !is_self_cycle
                        })
                        .map(|path| path.iter().map(|object_id| object_id.0).collect())
                        .collect();
                    let located_forms = match recovery {
                        RecoveryKind::NormalizedFormBBox => {
                            Some(&walked.normalized_form_object_ids)
                        }
                        RecoveryKind::ClippedFormContent => Some(&walked.clipped_form_object_ids),
                        _ => None,
                    };
                    let form_object_count = located_forms.map_or(0, BTreeSet::len);
                    let form_object_ids = located_forms
                        .into_iter()
                        .flatten()
                        .take(MAX_REPORTED_FORM_OBJECT_IDS)
                        .map(|object_id| object_id.0)
                        .collect();
                    document.diagnostics.push(Diagnostic::ContentRecovered {
                        page_index: page.index,
                        recovery,
                        form_cycle_paths,
                        form_object_ids,
                        form_object_count,
                    });
                }
                page.walked_characters = walked.characters;
                page.vector_paths = walked.vector_paths;
                page.path_ink = walked.path_ink;
                page.inline_images = walked.inline_images;
                page.content_streams = walked.content_streams;
            }
            Err(PageWalkError::Degraded { reason, .. }) => {
                degrade_page(page, &mut document.diagnostics, reason);
                continue;
            }
            // 能力边界（UnsupportedPdf）仍是文档级失败——那不是这一页坏了，
            // 而是 M1 还不会处理这类内容，降级会把「没实现」伪装成「文件有问题」。
            Err(PageWalkError::Fatal(error)) => return Err(error),
        }
        page.engine_characters = context
            .engine
            .page_characters(&document.original_bytes, page.index)?;
        page.character_alignment = validate_character_alignment(
            page.index,
            page.geometry,
            &page.walked_characters,
            &page.engine_characters,
            context.config.baseline_tolerance_pt,
            &mut document.diagnostics,
        );
    }
    Ok(())
}

pub fn layout(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let total_pages = document.extracted_pages.len();
    for page in &mut document.extracted_pages {
        if page.is_translatable() {
            let raster = context.engine.rasterize_page_at_scale(
                &document.original_bytes,
                page.index,
                context.layout_detector.raster_pixels_per_point(),
            )?;
            raster.validate()?;
            let synthetic_characters =
                if !page.recoveries.is_empty() || page.engine_characters.is_empty() {
                    reliable_upright_snapshots(&page.walked_characters)
                } else {
                    Vec::new()
                };
            let layout_characters = if synthetic_characters.is_empty() {
                &page.engine_characters
            } else {
                &synthetic_characters
            };
            page.layout_regions = context.layout_detector.detect(
                page.index,
                page.geometry,
                &raster,
                layout_characters,
            )?;
            add_fallback_regions(&mut page.layout_regions, &page.walked_characters);
            inherit_fallback_semantics(&mut page.layout_regions);
            apply_policy_overrides(
                page.geometry,
                &mut page.layout_regions,
                &page.walked_characters,
            );
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

fn add_fallback_regions(
    regions: &mut Vec<crate::engine::LayoutRegion>,
    walked: &[crate::walk::WalkedChar],
) {
    let mut candidates = walked
        .iter()
        .filter(|character| {
            character.visible
                && character.locatable
                && character.text_transform == TextTransform::Upright
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .baseline_origin
            .y
            .total_cmp(&left.baseline_origin.y)
            .then_with(|| left.baseline_origin.x.total_cmp(&right.baseline_origin.x))
            .then_with(|| left.byte_start.cmp(&right.byte_start))
    });

    let mut lines = Vec::<(Rect, f64, f64, f64, bool)>::new();
    for character in candidates {
        let baseline = character.baseline_origin.y;
        let max_baseline_delta = (character.font_size * 0.25).max(1.0);
        let max_gap = (character.font_size * 2.0).max(12.0);
        let center = crate::il::Point {
            x: (character.metric_box.left + character.metric_box.right) / 2.0,
            y: (character.metric_box.bottom + character.metric_box.top) / 2.0,
        };
        let uncovered = !regions
            .iter()
            .any(|region| point_in_rect(center, region.bounds));
        if let Some((bounds, line_baseline, right_edge, line_font_size, has_uncovered)) =
            lines.last_mut()
            && (baseline - *line_baseline).abs() <= max_baseline_delta.max(*line_font_size * 0.25)
            && character.metric_box.left - *right_edge <= max_gap.max(*line_font_size * 2.0)
        {
            *bounds = bounds.union(character.metric_box);
            *right_edge = (*right_edge).max(character.metric_box.right);
            *line_font_size = (*line_font_size).max(character.font_size);
            *has_uncovered |= uncovered;
        } else {
            lines.push((
                character.metric_box,
                baseline,
                character.metric_box.right,
                character.font_size,
                uncovered,
            ));
        }
    }

    let mut next_order = regions
        .iter()
        .map(|region| region.reading_order)
        .max()
        .map_or(0, |order| order.saturating_add(1));
    for (bounds, _, _, _, has_uncovered) in lines {
        if !has_uncovered {
            continue;
        }
        regions.push(crate::engine::LayoutRegion {
            bounds,
            reading_order: next_order,
            label: LayoutLabel::FallbackLine,
            source: LayoutSource::FallbackLine,
            confidence: 1.0,
        });
        next_order = next_order.saturating_add(1);
    }
}

fn apply_policy_overrides(
    geometry: PageGeometry,
    regions: &mut [crate::engine::LayoutRegion],
    walked: &[crate::walk::WalkedChar],
) {
    for region in regions
        .iter_mut()
        .filter(|region| region.label.translation_policy() == TranslationPolicy::Translate)
    {
        let text = walked
            .iter()
            .filter(|character| {
                character.visible
                    && character.locatable
                    && point_in_rect(character.baseline_origin, region.bounds)
            })
            .filter_map(|character| character.unicode)
            .collect::<String>();
        let trimmed = text.trim();
        let top_ratio = region.bounds.top / geometry.height;
        let bottom_ratio = region.bounds.bottom / geometry.height;
        let positional_apparatus = is_positional_apparatus(region.label, trimmed);
        if region.source == LayoutSource::Model && looks_like_numbered_caption(trimmed) {
            region.label = LayoutLabel::FigureTitle;
        } else if region.source == LayoutSource::Model && positional_apparatus && top_ratio >= 0.88
        {
            region.label = LayoutLabel::Header;
        } else if region.source == LayoutSource::Model
            && positional_apparatus
            && bottom_ratio <= 0.12
        {
            region.label = LayoutLabel::Footer;
        } else if looks_like_reference_entry(trimmed) {
            region.label = LayoutLabel::ReferenceContent;
        } else if region.bounds.top < geometry.height * 0.5 && looks_like_seal(trimmed) {
            region.label = LayoutLabel::Seal;
        }
    }
    recover_false_footer_body(regions, walked);
}

fn recover_false_footer_body(
    regions: &mut [crate::engine::LayoutRegion],
    walked: &[crate::walk::WalkedChar],
) {
    let recovered = regions
        .iter()
        .enumerate()
        .filter_map(|(index, footer)| {
            if footer.source != LayoutSource::Model || footer.label != LayoutLabel::Footer {
                return None;
            }
            let text = walked
                .iter()
                .filter(|character| {
                    character.visible
                        && character.locatable
                        && point_in_rect(character.baseline_origin, footer.bounds)
                })
                .filter_map(|character| character.unicode)
                .collect::<String>();
            if !looks_like_body_continuation(&text) {
                return None;
            }
            let previous = regions
                .iter()
                .filter(|candidate| {
                    candidate.source == LayoutSource::Model
                        && candidate.reading_order < footer.reading_order
                })
                .max_by_key(|candidate| candidate.reading_order)?;
            if previous.label != LayoutLabel::ParagraphTitle {
                return None;
            }
            let previous_height = previous.bounds.top - previous.bounds.bottom;
            let footer_height = footer.bounds.top - footer.bounds.bottom;
            let max_height = previous_height.max(footer_height);
            let vertical_gap = previous.bounds.bottom - footer.bounds.top;
            if !(0.0..=max_height * 1.55).contains(&vertical_gap)
                || (previous.bounds.left - footer.bounds.left).abs() > max_height
                || footer.bounds.right - footer.bounds.left <= footer_height * 4.0
            {
                return None;
            }
            regions
                .iter()
                .any(|candidate| {
                    candidate.source == LayoutSource::Model
                        && candidate.label == LayoutLabel::Number
                        && candidate.reading_order > footer.reading_order
                        && candidate.bounds.top < footer.bounds.bottom
                })
                .then_some(index)
        })
        .collect::<Vec<_>>();
    let recovered_bounds = recovered
        .iter()
        .map(|&index| regions[index].bounds)
        .collect::<Vec<_>>();
    for index in recovered {
        regions[index].label = LayoutLabel::Text;
    }
    for fallback in regions.iter_mut().filter(|region| {
        region.source == LayoutSource::FallbackLine && region.label == LayoutLabel::Footer
    }) {
        let center = crate::il::Point {
            x: (fallback.bounds.left + fallback.bounds.right) / 2.0,
            y: (fallback.bounds.bottom + fallback.bounds.top) / 2.0,
        };
        if recovered_bounds
            .iter()
            .any(|bounds| point_in_rect(center, *bounds))
        {
            fallback.label = LayoutLabel::Text;
        }
    }
}

fn looks_like_body_continuation(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed
        .chars()
        .filter(|character| !character.is_whitespace())
        .count()
        >= 20
        && trimmed.chars().any(char::is_lowercase)
        && trimmed
            .chars()
            .next_back()
            .is_some_and(|character| matches!(character, '.' | ':' | ';' | '?' | '!'))
        && !looks_like_numbered_caption(trimmed)
        && !looks_like_reference_entry(trimmed)
}

fn is_positional_apparatus(label: LayoutLabel, text: &str) -> bool {
    label == LayoutLabel::Text && text.chars().count() <= 100
}

fn looks_like_numbered_caption(text: &str) -> bool {
    let Some(rest) = text
        .strip_prefix("Table")
        .or_else(|| text.strip_prefix("Figure"))
    else {
        return false;
    };
    let rest = rest.trim_start();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && rest.as_bytes().get(digits) == Some(&b':')
}

fn looks_like_reference_entry(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('[') else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && rest.as_bytes().get(digits) == Some(&b']')
}

fn looks_like_seal(text: &str) -> bool {
    let mut letters = 0;
    for character in text.chars() {
        if character.is_alphabetic() {
            letters += 1;
            if !character.is_uppercase() {
                return false;
            }
        } else if !character.is_whitespace() && !character.is_ascii_punctuation() {
            return false;
        }
    }
    (3..=32).contains(&letters)
}

fn inherit_fallback_semantics(regions: &mut [crate::engine::LayoutRegion]) {
    let semantic_regions = regions.to_vec();
    for fallback in regions
        .iter_mut()
        .filter(|region| region.label == LayoutLabel::FallbackLine)
    {
        let center = crate::il::Point {
            x: (fallback.bounds.left + fallback.bounds.right) / 2.0,
            y: (fallback.bounds.bottom + fallback.bounds.top) / 2.0,
        };
        let owner = semantic_regions
            .iter()
            .filter(|region| {
                region.label != LayoutLabel::FallbackLine && point_in_rect(center, region.bounds)
            })
            .min_by(|left, right| {
                rect_area(left.bounds)
                    .total_cmp(&rect_area(right.bounds))
                    .then_with(|| left.reading_order.cmp(&right.reading_order))
            });
        if let Some(owner) = owner {
            fallback.label = owner.label;
        }
    }
}

fn point_in_rect(point: crate::il::Point, bounds: Rect) -> bool {
    point.x >= bounds.left
        && point.x <= bounds.right
        && point.y >= bounds.bottom
        && point.y <= bounds.top
}

fn rect_area(bounds: Rect) -> f64 {
    (bounds.right - bounds.left).max(0.0) * (bounds.top - bounds.bottom).max(0.0)
}

fn intersection_area(left: Rect, right: Rect) -> f64 {
    (left.right.min(right.right) - left.left.max(right.left)).max(0.0)
        * (left.top.min(right.top) - left.bottom.max(right.bottom)).max(0.0)
}

const SMALL_EDGE_CHARACTER_AREA_RATIO: f64 = 0.05;

fn median_metric_character_area(walked: &[crate::walk::WalkedChar]) -> Option<f64> {
    let mut areas = walked
        .iter()
        .filter(|character| {
            character.visible
                && character.locatable
                && character.text_transform == TextTransform::Upright
                && character
                    .unicode
                    .is_some_and(|unicode| !unicode.is_whitespace())
        })
        .map(|character| rect_area(character.metric_box))
        .filter(|area| area.is_finite() && *area > 0.0)
        .collect::<Vec<_>>();
    areas.sort_by(f64::total_cmp);
    areas.get(areas.len() / 2).copied()
}

fn layout_assignment(
    regions: &[crate::engine::LayoutRegion],
    bounds: Rect,
    small_edge_owner: Option<usize>,
    translate_table: bool,
) -> Option<LayoutAssignment> {
    let char_area = rect_area(bounds);
    regions
        .iter()
        .enumerate()
        .filter_map(|(index, region)| {
            let overlap = intersection_area(bounds, region.bounds);
            let center = crate::il::Point {
                x: (bounds.left + bounds.right) / 2.0,
                y: (bounds.bottom + bounds.top) / 2.0,
            };
            if overlap <= 0.0 && !point_in_rect(center, region.bounds) {
                return None;
            }
            let coverage = if char_area > 0.0 {
                overlap / char_area
            } else {
                0.0
            };
            Some((index, region, coverage))
        })
        .max_by(
            |(left_index, left, left_coverage), (right_index, right, right_coverage)| {
                layout_assignment_priority(*left_index, left, regions, bounds, small_edge_owner)
                    .cmp(&layout_assignment_priority(
                        *right_index,
                        right,
                        regions,
                        bounds,
                        small_edge_owner,
                    ))
                    .then_with(|| left_coverage.total_cmp(right_coverage))
                    .then_with(|| rect_area(right.bounds).total_cmp(&rect_area(left.bounds)))
                    .then_with(|| left.confidence.total_cmp(&right.confidence))
                    .then_with(|| right.reading_order.cmp(&left.reading_order))
            },
        )
        .map(|(_, region, _)| {
            let policy = if translate_table && region.label == LayoutLabel::Table {
                TranslationPolicy::Translate
            } else {
                region.label.translation_policy()
            };
            LayoutAssignment {
                label: region.label,
                reading_order: region.reading_order,
                bounds: region.bounds,
                source: region.source,
                policy,
            }
        })
}

fn small_edge_model_owner(
    regions: &[crate::engine::LayoutRegion],
    bounds: Rect,
    visual_bounds: Rect,
    median_metric_area: Option<f64>,
) -> Option<usize> {
    let median_metric_area = median_metric_area?;
    let visual_area = rect_area(visual_bounds);
    if !visual_area.is_finite()
        || visual_area <= 0.0
        || visual_area >= median_metric_area * SMALL_EDGE_CHARACTER_AREA_RATIO
        || !regions.iter().any(|region| {
            region.source == LayoutSource::FallbackLine
                && intersection_area(bounds, region.bounds) > 0.0
        })
    {
        return None;
    }
    let model_owners = regions
        .iter()
        .enumerate()
        .filter(|(_, region)| {
            region.source == LayoutSource::Model && intersection_area(bounds, region.bounds) > 0.0
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [owner] = model_owners.as_slice() else {
        return None;
    };
    let center = crate::il::Point {
        x: (bounds.left + bounds.right) / 2.0,
        y: (bounds.bottom + bounds.top) / 2.0,
    };
    (!point_in_rect(center, regions[*owner].bounds)).then_some(*owner)
}

fn layout_assignment_priority(
    region_index: usize,
    region: &crate::engine::LayoutRegion,
    regions: &[crate::engine::LayoutRegion],
    bounds: Rect,
    small_edge_owner: Option<usize>,
) -> u8 {
    match region.source {
        LayoutSource::FallbackLine => 1,
        LayoutSource::Model if small_edge_owner == Some(region_index) => 3,
        LayoutSource::Model
            if regions.iter().any(|fallback| {
                fallback.source == LayoutSource::FallbackLine
                    && fallback.label == region.label
                    && intersection_area(bounds, fallback.bounds) > 0.0
            }) =>
        {
            2
        }
        LayoutSource::Model => 0,
    }
}

fn reliable_upright_snapshots(walked: &[crate::walk::WalkedChar]) -> Vec<PageCharSnapshot> {
    walked
        .iter()
        .filter(|character| {
            character.visible
                && character.locatable
                && character.text_transform == TextTransform::Upright
                && character.font_supported
                && character.unicode.is_some()
                && character.advance.is_finite()
                && character.advance > 0.0
        })
        .enumerate()
        .filter_map(|(index, character)| {
            Some(PageCharSnapshot {
                index: u32::try_from(index).ok()?,
                unicode: character.unicode,
                unicode_value: character.unicode.map_or(0, u32::from),
                is_hyphen: None,
                baseline_origin: character.baseline_origin,
                tight_box: character.metric_box,
                loose_box: character.metric_box,
            })
        })
        .collect()
}

fn paragraph_preserved_reason<'a>(
    walked: impl IntoIterator<Item = (usize, &'a crate::walk::WalkedChar)>,
    weak_unicode_conflicts: &BTreeSet<usize>,
    has_only_form_owned_translatable_content: bool,
) -> Option<il::PreservedReason> {
    let translatable = walked
        .into_iter()
        .filter(|(_, character)| {
            character.visible && character.text_transform == TextTransform::Upright
        })
        .collect::<Vec<_>>();
    if translatable
        .iter()
        .any(|(_, character)| !character.font_supported)
    {
        Some(il::PreservedReason::UnsupportedFont)
    } else if has_only_form_owned_translatable_content {
        Some(il::PreservedReason::FormXObjectContent)
    } else if translatable.iter().any(|(index, character)| {
        character.unicode.is_none() || weak_unicode_conflicts.contains(index)
    }) {
        Some(il::PreservedReason::UnreliableUnicode)
    } else if translatable
        .iter()
        .any(|(_, character)| !character.advance.is_finite() || character.advance <= 0.0)
    {
        Some(il::PreservedReason::NonPositiveAdvance)
    } else if translatable
        .iter()
        .any(|(_, character)| !character.locatable)
    {
        Some(il::PreservedReason::Unlocatable)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
struct PositionedChar {
    walked_index: usize,
    locatable: bool,
    character: Char,
    force_no_space_before: bool,
    small_edge_character: bool,
}

#[derive(Debug, Clone)]
struct TextLine {
    chars: Vec<PositionedChar>,
    bounds: Rect,
    baseline: f64,
    font_size: f64,
    numeric_apparatus: bool,
}

#[derive(Debug)]
struct ModelGroup {
    assignment: LayoutAssignment,
    chars: Vec<PositionedChar>,
}

#[derive(Debug)]
struct ParagraphDraft {
    model_order: Option<usize>,
    apparatus: bool,
    column_left: f64,
    top: f64,
    lines: Vec<TextLine>,
}

pub fn paragraph_find(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let mut pages = Vec::with_capacity(document.extracted_pages.len());
    let mut form_wrapper_pages = Vec::with_capacity(document.extracted_pages.len());
    let mut unicode_recoveries = Vec::new();
    let mut glyph_bbox_estimates = Vec::new();
    for extracted in &document.extracted_pages {
        if !extracted.is_translatable() {
            pages.push(il::Page {
                index: extracted.index,
                geometry: extracted.geometry,
                paragraphs: Vec::new(),
            });
            form_wrapper_pages.push((false, false));
            continue;
        }
        if extracted.walked_characters.is_empty() && extracted.layout_regions.is_empty() {
            pages.push(il::Page {
                index: extracted.index,
                geometry: extracted.geometry,
                paragraphs: Vec::new(),
            });
            form_wrapper_pages.push((false, false));
            continue;
        }
        let median_metric_area = median_metric_character_area(&extracted.walked_characters);
        let positioned = extracted
            .walked_characters
            .iter()
            .enumerate()
            .map(|(index, walked)| {
                let visual_bbox = walked.estimated_bbox.unwrap_or_else(|| {
                    extracted
                        .character_alignment
                        .engine_indices_by_walk
                        .get(index)
                        .and_then(|engine_index| *engine_index)
                        .and_then(|engine_index| extracted.engine_characters.get(engine_index))
                        .filter(|_| walked.locatable)
                        .map_or(walked.metric_box, |engine| engine.tight_box)
                });
                if walked.estimated_bbox.is_some() {
                    glyph_bbox_estimates.push(Diagnostic::GlyphBboxEstimated {
                        page_index: extracted.index,
                        character_index: index,
                        font_object: [walked.font.object_number, u32::from(walked.font.generation)],
                        code: walked.code,
                    });
                }
                let small_edge_owner = (walked.visible
                    && walked.locatable
                    && walked
                        .unicode
                        .is_some_and(|unicode| !unicode.is_whitespace()))
                .then(|| {
                    small_edge_model_owner(
                        &extracted.layout_regions,
                        walked.metric_box,
                        visual_bbox,
                        median_metric_area,
                    )
                })
                .flatten();
                PositionedChar {
                    walked_index: index,
                    locatable: walked.locatable,
                    force_no_space_before: false,
                    small_edge_character: small_edge_owner.is_some(),
                    character: Char {
                        unicode: walked.unicode,
                        unicode_source: (walked.unicode_provenance
                            == UnicodeProvenance::DifferencesAgl)
                            .then_some(il::UnicodeSource::DifferencesAgl),
                        code: walked.code,
                        visible: walked.visible,
                        font: walked.font.clone(),
                        font_size: page_space_font_size(walked),
                        baseline_origin: walked.baseline_origin,
                        r#box: walked.metric_box,
                        visual_bbox,
                        bbox_estimated: walked.estimated_bbox.is_some(),
                        text_transform: walked.text_transform,
                        implicit_space_before: false,
                        layout: layout_assignment(
                            &extracted.layout_regions,
                            walked.metric_box,
                            small_edge_owner,
                            context.config.translate_table,
                        ),
                        passthrough: PassthroughRef {
                            content_object: walked.content_object.0,
                            byte_start: walked.byte_start,
                            byte_end: walked.byte_end,
                            encoded: walked.encoded.clone(),
                        },
                    },
                }
            })
            .collect::<Vec<_>>();

        let mut model_groups = Vec::<ModelGroup>::new();
        let mut fallback = Vec::new();
        let mut invisible = Vec::new();
        let mut isolated = Vec::new();
        for positioned in positioned {
            if !positioned.character.visible && positioned.locatable {
                invisible.push(positioned);
                continue;
            }
            if !positioned.locatable {
                isolated.push(positioned);
                continue;
            }
            let model_assignment = positioned
                .character
                .layout
                .filter(|assignment| assignment.source == LayoutSource::Model);
            if let Some(assignment) = model_assignment {
                if let Some(group) = model_groups
                    .iter_mut()
                    .find(|group| group.assignment == assignment)
                {
                    group.chars.push(positioned);
                } else {
                    model_groups.push(ModelGroup {
                        assignment,
                        chars: vec![positioned],
                    });
                }
            } else {
                fallback.push(positioned);
            }
        }

        merge_nested_inline_formula_groups(&mut model_groups);
        model_groups.sort_by_key(|group| group.assignment.reading_order);
        let mut drafts = Vec::new();
        for mut group in model_groups {
            if group.assignment.label == LayoutLabel::ParagraphTitle {
                mark_leading_section_number(&mut group.chars);
            }
            if group.assignment.label == LayoutLabel::AsideText
                && chars_are_narrow_number(&group.chars)
            {
                mark_chars_as_number(&mut group.chars);
            }
            let mut lines = build_text_lines(group.chars);
            merge_toc_page_numbers(&mut lines);
            let columns = match group.assignment.label {
                LayoutLabel::Text => split_parallel_model_columns(lines),
                LayoutLabel::Abstract => split_model_abstract_columns(lines),
                _ => vec![lines],
            };
            if group.assignment.label == LayoutLabel::Abstract && columns.len() > 1 {
                let paragraph_lines = columns.into_iter().flatten().collect::<Vec<_>>();
                let bounds = lines_bounds(&paragraph_lines);
                drafts.push(ParagraphDraft {
                    model_order: Some(group.assignment.reading_order),
                    apparatus: false,
                    column_left: bounds.left,
                    top: bounds.top,
                    lines: paragraph_lines,
                });
                continue;
            }
            for column in columns {
                for paragraph_lines in split_natural_paragraphs(column, group.assignment.label) {
                    let bounds = lines_bounds(&paragraph_lines);
                    drafts.push(ParagraphDraft {
                        model_order: Some(group.assignment.reading_order),
                        apparatus: paragraph_lines.iter().all(line_is_number),
                        column_left: bounds.left,
                        top: bounds.top,
                        lines: paragraph_lines,
                    });
                }
            }
        }

        let mut fallback_lines = build_text_lines(fallback);
        merge_toc_page_numbers(&mut fallback_lines);
        mark_numeric_apparatus(&mut fallback_lines);
        for column in group_lines_into_columns(fallback_lines) {
            for paragraph_lines in split_natural_paragraphs(column, LayoutLabel::FallbackLine) {
                let bounds = lines_bounds(&paragraph_lines);
                drafts.push(ParagraphDraft {
                    model_order: None,
                    apparatus: paragraph_lines.iter().all(|line| line.numeric_apparatus),
                    column_left: bounds.left,
                    top: bounds.top,
                    lines: paragraph_lines,
                });
            }
        }
        for line in invisible_text_show_lines(invisible) {
            drafts.push(ParagraphDraft {
                model_order: None,
                apparatus: false,
                column_left: line.bounds.left,
                top: line.bounds.top,
                lines: vec![line],
            });
        }
        attach_isolated_chars(&mut drafts, isolated);

        drafts.sort_by(|left, right| match (left.model_order, right.model_order) {
            (Some(left_order), Some(right_order)) => left_order
                .cmp(&right_order)
                .then_with(|| right.top.total_cmp(&left.top))
                .then_with(|| left.column_left.total_cmp(&right.column_left)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => right
                .apparatus
                .cmp(&left.apparatus)
                .then_with(|| {
                    if left.apparatus && right.apparatus {
                        right.top.total_cmp(&left.top)
                    } else {
                        left.column_left.total_cmp(&right.column_left)
                    }
                })
                .then_with(|| right.top.total_cmp(&left.top)),
        });
        let page_content_objects = extracted
            .content_streams
            .iter()
            .map(|stream| stream.object_id.0)
            .collect::<BTreeSet<_>>();
        let mut paragraphs = drafts
            .into_iter()
            .enumerate()
            .map(|(reading_order, draft)| {
                paragraph_from_lines(
                    reading_order,
                    draft.lines,
                    &extracted.walked_characters,
                    &extracted.character_alignment.weak_unicode_conflicts,
                    &page_content_objects,
                )
            })
            .collect::<Vec<_>>();
        if extracted.index == 0 {
            apply_title_author_passthrough(&mut paragraphs);
        }
        for paragraph in &mut paragraphs {
            if !paragraph
                .chars()
                .iter()
                .all(first_line_indent_character_is_prose)
            {
                paragraph.first_line_indent = None;
            }
        }
        for (paragraph_index, paragraph) in paragraphs.iter().enumerate() {
            let recovered_character_count = paragraph
                .chars()
                .iter()
                .filter(|character| {
                    character.unicode_source == Some(il::UnicodeSource::DifferencesAgl)
                })
                .count();
            if recovered_character_count > 0 {
                unicode_recoveries.push(Diagnostic::UnicodeRecovered {
                    page_index: extracted.index,
                    paragraph_index,
                    reading_order: paragraph.reading_order,
                    recovered_character_count,
                });
            }
        }
        let visible_upright = paragraphs
            .iter()
            .flat_map(Paragraph::chars)
            .filter(|character| {
                character.visible && character.text_transform == TextTransform::Upright
            })
            .collect::<Vec<_>>();
        let is_form_wrapper_page = !visible_upright.is_empty()
            && visible_upright.iter().all(|character| {
                !page_content_objects.contains(&character.passthrough.content_object)
            });
        let has_translatable_content = visible_upright.iter().any(|character| {
            character
                .layout
                .is_some_and(|layout| layout.policy == TranslationPolicy::Translate)
        });
        form_wrapper_pages.push((is_form_wrapper_page, has_translatable_content));
        pages.push(il::Page {
            index: extracted.index,
            geometry: extracted.geometry,
            paragraphs,
        });
    }
    let document_has_translatable_form_wrapper = form_wrapper_pages
        .iter()
        .any(|(is_wrapper, has_translatable)| *is_wrapper && *has_translatable);
    if document_has_translatable_form_wrapper {
        for (page, (is_wrapper, _)) in pages.iter_mut().zip(&form_wrapper_pages) {
            if !is_wrapper {
                continue;
            }
            for paragraph in &mut page.paragraphs {
                if paragraph.preserved != Some(il::PreservedReason::UnsupportedFont) {
                    paragraph.preserved = Some(il::PreservedReason::FormXObjectContent);
                }
            }
        }
    }
    document.il = il::Document {
        schema_version: il::SCHEMA_VERSION,
        pages,
        publication_ink: Vec::new(),
    };
    for diagnostic in unicode_recoveries {
        document.diagnostics.push(diagnostic);
    }
    for diagnostic in glyph_bbox_estimates {
        document.diagnostics.push(diagnostic);
    }
    Ok(())
}

fn page_space_font_size(walked: &crate::walk::WalkedChar) -> f64 {
    let [a, b, c, d, _, _] = walked.content_transform;
    let [_, _, text_c, text_d, _, _] = walked.text_matrix_before_glyph;
    let vertical_x = a * text_c + c * text_d;
    let vertical_y = b * text_c + d * text_d;
    let effective = walked.font_size.abs() * vertical_x.hypot(vertical_y);
    if effective.is_finite() && effective > 0.0 {
        effective
    } else {
        walked.font_size
    }
}

fn apply_title_author_passthrough(paragraphs: &mut [Paragraph]) {
    let Some(title_index) = paragraphs
        .iter()
        .position(|paragraph| paragraph_has_only_label(paragraph, LayoutLabel::DocTitle))
    else {
        return;
    };
    let title = &paragraphs[title_index];
    let Some(lower_index) = paragraphs
        .iter()
        .enumerate()
        .filter(|(_, paragraph)| {
            paragraph_has_only_label(paragraph, LayoutLabel::Abstract)
                || paragraph_has_only_label(paragraph, LayoutLabel::ParagraphTitle)
        })
        .filter(|(_, paragraph)| paragraph.bounds.top < title.bounds.bottom)
        .max_by(|(_, left), (_, right)| left.bounds.top.total_cmp(&right.bounds.top))
        .map(|(index, _)| index)
    else {
        return;
    };
    let lower = &paragraphs[lower_index];
    let Some(band) = mimus_quality_contract::title_author_band(
        title.bounds.bottom,
        lower.bounds.top,
        title
            .chars()
            .iter()
            .chain(lower.chars())
            .map(|character| character.font_size),
    ) else {
        return;
    };

    for paragraph in paragraphs {
        if !(paragraph_has_only_label(paragraph, LayoutLabel::Text)
            || paragraph_has_only_label(paragraph, LayoutLabel::FallbackLine))
            || !band.contains(paragraph.bounds.bottom, paragraph.bounds.top)
        {
            continue;
        }
        for character in paragraph_chars_mut(paragraph) {
            if let Some(layout) = &mut character.layout {
                layout.policy = TranslationPolicy::Passthrough;
            }
        }
    }
}

fn paragraph_has_only_label(paragraph: &Paragraph, label: LayoutLabel) -> bool {
    !paragraph.chars().is_empty()
        && paragraph
            .chars()
            .iter()
            .all(|character| character.layout.is_some_and(|layout| layout.label == label))
}

fn paragraph_chars_mut(paragraph: &mut Paragraph) -> &mut [Char] {
    match &mut paragraph.text {
        TextCarrier::Chars { chars } => chars,
    }
}

fn mark_leading_section_number(chars: &mut [PositionedChar]) {
    let mut visual_order = (0..chars.len()).collect::<Vec<_>>();
    visual_order.sort_by(|&left, &right| {
        chars[left]
            .character
            .r#box
            .left
            .total_cmp(&chars[right].character.r#box.left)
            .then(left.cmp(&right))
    });
    let Some(marker_kind) = visual_order.iter().find_map(|&index| {
        chars[index]
            .character
            .unicode
            .filter(|value| !value.is_whitespace())
            .map(|value| value.is_ascii_digit())
    }) else {
        return;
    };
    let candidates = visual_order
        .iter()
        .copied()
        .take_while(|&index| {
            chars[index].character.unicode.is_some_and(|value| {
                (if marker_kind {
                    value.is_ascii_digit()
                } else {
                    matches!(value, 'I' | 'V' | 'X')
                }) || value == '.'
                    || value.is_whitespace()
            })
        })
        .collect::<Vec<_>>();
    if !candidates.iter().any(|&index| {
        chars[index]
            .character
            .unicode
            .is_some_and(|value| value.is_ascii_digit() || matches!(value, 'I' | 'V' | 'X'))
    }) {
        return;
    }
    let Some(title) = visual_order
        .iter()
        .copied()
        .skip(candidates.len())
        .find(|&index| {
            chars[index]
                .character
                .unicode
                .is_some_and(char::is_alphabetic)
        })
    else {
        return;
    };
    let title_left = chars[title].character.r#box.left;
    let candidate_right = candidates
        .iter()
        .map(|&index| chars[index].character.r#box.right)
        .max_by(f64::total_cmp)
        .unwrap_or(title_left);
    let max_font_size = candidates
        .iter()
        .map(|&index| chars[index].character.font_size)
        .fold(chars[title].character.font_size, f64::max);
    let horizontal_gap = title_left - candidate_right;
    let baseline_aligned = candidates.iter().all(|&index| {
        let candidate = &chars[index];
        (chars[title].character.baseline_origin.y - candidate.character.baseline_origin.y).abs()
            <= chars[title]
                .character
                .font_size
                .max(candidate.character.font_size)
                * 0.35
    });
    if !baseline_aligned
        || horizontal_gap < max_font_size * 0.25
        || horizontal_gap > max_font_size * 2.0
    {
        return;
    }
    for index in candidates {
        if let Some(layout) = &mut chars[index].character.layout {
            layout.label = LayoutLabel::Number;
            layout.policy = TranslationPolicy::Passthrough;
        }
    }
    chars[title].force_no_space_before = true;
}

fn merge_nested_inline_formula_groups(groups: &mut Vec<ModelGroup>) {
    let owners = groups
        .iter()
        .enumerate()
        .map(|(formula_index, formula)| {
            if formula.assignment.label != LayoutLabel::InlineFormula {
                return None;
            }
            let formula_bounds = formula.assignment.bounds;
            let formula_center = crate::il::Point {
                x: (formula_bounds.left + formula_bounds.right) / 2.0,
                y: (formula_bounds.bottom + formula_bounds.top) / 2.0,
            };
            let formula_area = rect_area(formula_bounds);
            groups
                .iter()
                .enumerate()
                .filter(|(owner_index, owner)| {
                    *owner_index != formula_index
                        && owner.assignment.source == LayoutSource::Model
                        && owner.assignment.policy == TranslationPolicy::Translate
                        && rect_area(owner.assignment.bounds) > formula_area
                        && point_in_rect(formula_center, owner.assignment.bounds)
                })
                .min_by(|(_, left), (_, right)| {
                    rect_area(left.assignment.bounds)
                        .total_cmp(&rect_area(right.assignment.bounds))
                        .then_with(|| {
                            left.assignment
                                .reading_order
                                .cmp(&right.assignment.reading_order)
                        })
                })
                .map(|(owner_index, _)| owner_index)
        })
        .collect::<Vec<_>>();

    for (formula_index, owner_index) in owners.into_iter().enumerate() {
        let Some(owner_index) = owner_index else {
            continue;
        };
        let chars = std::mem::take(&mut groups[formula_index].chars);
        groups[owner_index].chars.extend(chars);
    }
    groups.retain(|group| !group.chars.is_empty());
}

fn build_text_lines(chars: Vec<PositionedChar>) -> Vec<TextLine> {
    let (small_edge_characters, mut chars): (Vec<_>, Vec<_>) = chars
        .into_iter()
        .partition(|character| character.small_edge_character);
    chars.sort_by(|left, right| {
        right
            .character
            .baseline_origin
            .y
            .total_cmp(&left.character.baseline_origin.y)
            .then_with(|| {
                left.character
                    .baseline_origin
                    .x
                    .total_cmp(&right.character.baseline_origin.x)
            })
            .then_with(|| left.walked_index.cmp(&right.walked_index))
    });
    let mut rows = Vec::<Vec<PositionedChar>>::new();
    for character in chars {
        let belongs = rows.last().is_some_and(|row| {
            let first = &row[0].character;
            (first.baseline_origin.y - character.character.baseline_origin.y).abs()
                <= first.font_size.max(character.character.font_size) * 0.35
        });
        if belongs {
            rows.last_mut().unwrap().push(character);
        } else {
            rows.push(vec![character]);
        }
    }
    // Raised edge-owned marks bypass baseline clustering, then rejoin only their adjacent source
    // glyph so the paragraph keeps inline source order.
    for mut character in small_edge_characters {
        let owner = rows
            .iter()
            .enumerate()
            .filter_map(|(row_index, row)| {
                row.iter()
                    .filter(|candidate| {
                        candidate.character.passthrough.content_object
                            == character.character.passthrough.content_object
                            && candidate.walked_index.abs_diff(character.walked_index) == 1
                            && rects_overlap_vertically(
                                candidate.character.r#box,
                                character.character.r#box,
                            )
                    })
                    .map(|candidate| {
                        let horizontal_gap = (character.character.r#box.left
                            - candidate.character.r#box.right)
                            .max(candidate.character.r#box.left - character.character.r#box.right)
                            .max(0.0);
                        (
                            row_index,
                            candidate.walked_index.abs_diff(character.walked_index),
                            horizontal_gap,
                            candidate
                                .character
                                .font_size
                                .max(character.character.font_size),
                        )
                    })
                    .filter(|(_, _, gap, font_size)| *gap <= *font_size * 0.25)
                    .min_by(|left, right| {
                        left.1
                            .cmp(&right.1)
                            .then_with(|| left.2.total_cmp(&right.2))
                    })
            })
            .min_by(|left, right| {
                left.1
                    .cmp(&right.1)
                    .then_with(|| left.2.total_cmp(&right.2))
            })
            .map(|(row_index, _, _, _)| row_index);
        if let Some(owner) = owner {
            character.force_no_space_before = true;
            rows[owner].push(character);
        } else {
            rows.push(vec![character]);
        }
    }

    let mut lines = Vec::new();
    for mut row in rows {
        row.sort_by(|left, right| {
            left.character
                .baseline_origin
                .x
                .total_cmp(&right.character.baseline_origin.x)
                .then_with(|| left.walked_index.cmp(&right.walked_index))
        });
        let mut segment = Vec::new();
        for character in row {
            let split = segment.last().is_some_and(|previous: &PositionedChar| {
                let gap = character.character.r#box.left - previous.character.r#box.right;
                let font_size = previous
                    .character
                    .font_size
                    .max(character.character.font_size);
                let leading_number = !segment.is_empty()
                    && segment.iter().all(|value: &PositionedChar| {
                        value
                            .character
                            .unicode
                            .is_some_and(|unicode| unicode.is_ascii_digit())
                    });
                let retained_section_number = leading_number
                    && segment.iter().all(|value| {
                        value.character.layout.is_some_and(|number| {
                            character.character.layout.is_some_and(|title| {
                                number.label == LayoutLabel::Number
                                    && title.label == LayoutLabel::ParagraphTitle
                                    && number.reading_order == title.reading_order
                                    && number.bounds == title.bounds
                                    && number.source == title.source
                            })
                        })
                    });
                gap > font_size * 1.8
                    || (leading_number && !retained_section_number && gap > font_size * 0.8)
            });
            if split {
                lines.push(text_line(std::mem::take(&mut segment)));
            }
            segment.push(character);
        }
        if !segment.is_empty() {
            lines.push(text_line(segment));
        }
    }
    lines
}

fn invisible_text_show_lines(mut chars: Vec<PositionedChar>) -> Vec<TextLine> {
    chars.sort_by_key(|character| character.walked_index);
    let mut runs = Vec::<Vec<PositionedChar>>::new();
    for character in chars {
        let same_text_show = runs
            .last()
            .and_then(|run| run.last())
            .is_some_and(|previous| {
                previous.character.passthrough.content_object
                    == character.character.passthrough.content_object
                    && previous.character.passthrough.byte_start
                        == character.character.passthrough.byte_start
                    && previous.character.passthrough.byte_end
                        == character.character.passthrough.byte_end
            });
        if same_text_show {
            runs.last_mut().unwrap().push(character);
        } else {
            runs.push(vec![character]);
        }
    }
    runs.into_iter().map(text_line).collect()
}

fn text_line(chars: Vec<PositionedChar>) -> TextLine {
    let first = &chars[0].character;
    TextLine {
        bounds: chars[1..].iter().fold(first.r#box, |bounds, character| {
            bounds.union(character.character.r#box)
        }),
        baseline: chars
            .iter()
            .map(|character| character.character.baseline_origin.y)
            .sum::<f64>()
            / chars.len() as f64,
        font_size: chars
            .iter()
            .map(|character| character.character.font_size)
            .fold(0.0, f64::max),
        chars,
        numeric_apparatus: false,
    }
}

fn line_text(line: &TextLine) -> String {
    line.chars
        .iter()
        .filter_map(|character| character.character.unicode)
        .collect()
}

fn line_is_number(line: &TextLine) -> bool {
    let text = line_text(line);
    !text.is_empty() && text.chars().all(|character| character.is_ascii_digit())
}

fn merge_toc_page_numbers(lines: &mut Vec<TextLine>) {
    let pairs = lines
        .iter()
        .filter(|number| {
            line_is_number(number)
                && lines.iter().any(|text| {
                    !line_is_number(text)
                        && text.bounds.right < number.bounds.left
                        && (text.baseline - number.baseline).abs()
                            <= text.font_size.max(number.font_size) * 0.35
                })
        })
        .count();
    if pairs < 2 {
        return;
    }

    let mut number_indices = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line_is_number(line).then_some(index))
        .collect::<Vec<_>>();
    number_indices.sort_unstable_by(|left, right| right.cmp(left));
    for number_index in number_indices {
        let number = lines.remove(number_index);
        let owner = lines
            .iter()
            .enumerate()
            .filter(|(_, text)| {
                !line_is_number(text)
                    && text.bounds.right < number.bounds.left
                    && (text.baseline - number.baseline).abs()
                        <= text.font_size.max(number.font_size) * 0.35
            })
            .max_by(|(_, left), (_, right)| left.bounds.right.total_cmp(&right.bounds.right))
            .map(|(index, _)| index);
        if let Some(owner) = owner {
            let mut number_chars = number.chars;
            if let Some(first) = number_chars.first_mut() {
                first.force_no_space_before = true;
            }
            let mut chars = std::mem::take(&mut lines[owner].chars);
            chars.extend(number_chars);
            chars.sort_by(|left, right| {
                left.character
                    .baseline_origin
                    .x
                    .total_cmp(&right.character.baseline_origin.x)
            });
            lines[owner] = text_line(chars);
        } else {
            lines.push(number);
        }
    }
}

fn chars_are_narrow_number(chars: &[PositionedChar]) -> bool {
    let mut text = String::new();
    let mut bounds = None;
    let mut font_size = 0.0_f64;
    for character in chars {
        if let Some(unicode) = character.character.unicode {
            text.push(unicode);
        }
        bounds = Some(bounds.map_or(character.character.r#box, |value: Rect| {
            value.union(character.character.r#box)
        }));
        font_size = font_size.max(character.character.font_size);
    }
    !text.is_empty()
        && text.chars().all(|character| character.is_ascii_digit())
        && bounds.is_some_and(|bounds| bounds.right - bounds.left <= font_size * 4.0)
}

fn mark_chars_as_number(chars: &mut [PositionedChar]) {
    for positioned in chars {
        if let Some(layout) = &mut positioned.character.layout {
            layout.label = LayoutLabel::Number;
            layout.policy = TranslationPolicy::Passthrough;
        }
    }
}

fn mark_numeric_apparatus(lines: &mut [TextLine]) {
    let apparatus = lines
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            (line_is_number(candidate)
                && candidate.bounds.right - candidate.bounds.left <= candidate.font_size * 4.0
                && lines.iter().any(|body| {
                    !line_is_number(body)
                        && body.bounds.left > candidate.bounds.right
                        && (body.baseline - candidate.baseline).abs()
                            <= body.font_size.max(candidate.font_size) * 0.35
                }))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    for index in apparatus {
        lines[index].numeric_apparatus = true;
        mark_chars_as_number(&mut lines[index].chars);
    }
}

fn group_lines_into_columns(mut lines: Vec<TextLine>) -> Vec<Vec<TextLine>> {
    lines.sort_by(|left, right| {
        left.bounds
            .left
            .total_cmp(&right.bounds.left)
            .then_with(|| right.bounds.top.total_cmp(&left.bounds.top))
    });
    let mut columns = Vec::<Vec<TextLine>>::new();
    for line in lines {
        let owner = columns.iter().position(|column| {
            let anchor = &column[0];
            anchor.numeric_apparatus == line.numeric_apparatus
                && (anchor.bounds.left - line.bounds.left).abs()
                    <= anchor.font_size.max(line.font_size) * 2.5
        });
        if let Some(owner) = owner {
            columns[owner].push(line);
        } else {
            columns.push(vec![line]);
        }
    }
    columns.sort_by(|left, right| left[0].bounds.left.total_cmp(&right[0].bounds.left));
    for column in &mut columns {
        column.sort_by(|left, right| {
            right
                .baseline
                .total_cmp(&left.baseline)
                .then_with(|| left.bounds.left.total_cmp(&right.bounds.left))
        });
    }
    columns
}

fn split_model_abstract_columns(lines: Vec<TextLine>) -> Vec<Vec<TextLine>> {
    let columns = group_lines_into_columns(lines.clone());
    if columns.len() == 2 && columns.iter().all(|column| column.len() >= 2) {
        columns
    } else {
        vec![lines]
    }
}

fn split_parallel_model_columns(lines: Vec<TextLine>) -> Vec<Vec<TextLine>> {
    let Some(separator) = parallel_model_column_separator(&lines) else {
        return vec![lines];
    };
    if lines.iter().any(|line| {
        line.chars.iter().any(|character| {
            let bounds = character.character.r#box;
            bounds.left < separator && bounds.right > separator
        })
    }) {
        return vec![lines];
    }
    let left_line_count = lines
        .iter()
        .filter(|line| {
            line.chars
                .iter()
                .any(|character| character.character.r#box.right <= separator)
        })
        .count();
    let right_line_count = lines
        .iter()
        .filter(|line| {
            line.chars
                .iter()
                .any(|character| character.character.r#box.left >= separator)
        })
        .count();
    let left_right = lines
        .iter()
        .flat_map(|line| &line.chars)
        .filter(|character| character.character.r#box.right <= separator)
        .map(|character| character.character.r#box.right)
        .max_by(f64::total_cmp);
    let right_left = lines
        .iter()
        .flat_map(|line| &line.chars)
        .filter(|character| character.character.r#box.left >= separator)
        .map(|character| character.character.r#box.left)
        .min_by(f64::total_cmp);
    if left_line_count < 2
        || right_line_count < 2
        || left_right
            .zip(right_left)
            .is_none_or(|(left, right)| left >= right)
    {
        return vec![lines];
    }

    let mut left = Vec::new();
    let mut right = Vec::new();
    for line in lines {
        let mut left_chars = Vec::new();
        let mut right_chars = Vec::new();
        for character in line.chars {
            let bounds = character.character.r#box;
            if bounds.right <= separator {
                left_chars.push(character);
            } else {
                right_chars.push(character);
            }
        }
        if !left_chars.is_empty() {
            left.push(text_line(left_chars));
        }
        if !right_chars.is_empty() {
            right.push(text_line(right_chars));
        }
    }
    bound_model_column(&mut left, |bounds| {
        bounds.right = bounds.right.min(separator);
    });
    bound_model_column(&mut right, |bounds| {
        bounds.left = bounds.left.max(separator);
    });
    vec![left, right]
}

fn bound_model_column(lines: &mut [TextLine], update: impl Fn(&mut Rect)) {
    for character in lines.iter_mut().flat_map(|line| &mut line.chars) {
        if let Some(layout) = &mut character.character.layout {
            update(&mut layout.bounds);
        }
    }
}

fn parallel_model_column_separator(lines: &[TextLine]) -> Option<f64> {
    let mut rows = Vec::<Vec<&TextLine>>::new();
    let mut ordered = lines.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        right
            .baseline
            .total_cmp(&left.baseline)
            .then_with(|| left.bounds.left.total_cmp(&right.bounds.left))
    });
    for line in ordered {
        let belongs = rows.last().is_some_and(|row| {
            let anchor = row[0];
            (anchor.baseline - line.baseline).abs() <= anchor.font_size.max(line.font_size) * 0.35
        });
        if belongs {
            rows.last_mut()?.push(line);
        } else {
            rows.push(vec![line]);
        }
    }

    let mut gutters = Vec::new();
    for row in rows {
        let mut substantial = row
            .into_iter()
            .filter(|line| {
                line.bounds.right - line.bounds.left >= line.font_size * 0.75
                    && line.chars.iter().all(|character| {
                        character
                            .character
                            .layout
                            .is_some_and(|layout| layout.label == LayoutLabel::Text)
                    })
            })
            .collect::<Vec<_>>();
        if substantial.len() != 2 {
            continue;
        }
        substantial.sort_by(|left, right| left.bounds.left.total_cmp(&right.bounds.left));
        let left = substantial[0];
        let right = substantial[1];
        let gap = right.bounds.left - left.bounds.right;
        if gap > left.font_size.max(right.font_size) * 1.8 {
            gutters.push((left.bounds.right, right.bounds.left));
        }
    }
    if gutters.len() < 2 {
        return None;
    }
    let common_left = gutters
        .iter()
        .map(|(left, _)| *left)
        .max_by(f64::total_cmp)?;
    let common_right = gutters
        .iter()
        .map(|(_, right)| *right)
        .min_by(f64::total_cmp)?;
    (common_left < common_right).then_some((common_left + common_right) / 2.0)
}

fn split_natural_paragraphs(mut lines: Vec<TextLine>, label: LayoutLabel) -> Vec<Vec<TextLine>> {
    if lines.is_empty() {
        return Vec::new();
    }
    lines.sort_by(|left, right| {
        right
            .baseline
            .total_cmp(&left.baseline)
            .then_with(|| left.bounds.left.total_cmp(&right.bounds.left))
    });
    let toc_like = lines.len() >= 3
        && lines
            .iter()
            .filter(|line| {
                line_text(line)
                    .trim_end()
                    .ends_with(|character: char| character.is_ascii_digit())
            })
            .count()
            * 5
            >= lines.len() * 4;
    if label == LayoutLabel::Table {
        infer_table_cell_bounds(&mut lines);
        return lines.into_iter().map(|line| vec![line]).collect();
    }
    if label == LayoutLabel::Content || toc_like {
        return lines.into_iter().map(|line| vec![line]).collect();
    }

    let mut steps = lines
        .windows(2)
        .filter_map(|pair| {
            let step = pair[0].baseline - pair[1].baseline;
            let font_size = pair[0].font_size.max(pair[1].font_size);
            (step >= font_size * 0.5 && step <= font_size * 2.0).then_some(step)
        })
        .collect::<Vec<_>>();
    steps.sort_by(f64::total_cmp);
    let typical = steps
        .get(steps.len() / 2)
        .copied()
        .unwrap_or(lines[0].font_size * 1.2);
    let indent_boundary_eligible = label == LayoutLabel::Text
        && lines.iter().flat_map(|line| &line.chars).all(|positioned| {
            positioned.character.layout.is_some_and(|layout| {
                layout.source == LayoutSource::Model
                    && layout.label == LayoutLabel::Text
                    && layout.policy == TranslationPolicy::Translate
            })
        });
    if !indent_boundary_eligible {
        return split_at_legacy_indent_boundaries(lines, typical);
    }
    let container = lines
        .iter()
        .flat_map(|line| &line.chars)
        .filter_map(|positioned| positioned.character.layout.map(|layout| layout.bounds))
        .reduce(Rect::union)
        .unwrap_or_else(|| lines_bounds(&lines));
    let available_width = (container.right - container.left).max(0.0);
    let mut line_starts = lines
        .iter()
        .filter_map(line_baseline_start)
        .collect::<Vec<_>>();
    line_starts.sort_by(f64::total_cmp);
    let normal_line_start = line_starts
        .get(line_starts.len().saturating_sub(1) / 2)
        .copied();
    let mut paragraphs = vec![vec![lines.remove(0)]];
    for line in lines {
        let previous = paragraphs.last().unwrap().last().unwrap();
        let gap = previous.baseline - line.baseline;
        let previous_width = previous.bounds.right - previous.bounds.left;
        let first_line_indented = normal_line_start
            .zip(line_baseline_start(&line))
            .is_some_and(|(normal, start)| start - normal > line.font_size * 1.2);
        let previous_line_underfilled =
            available_width > 0.0 && previous_width <= available_width * 0.8 + 0.01;
        let next_visual_line = gap >= line.font_size * 0.5;
        if gap > typical * 1.55
            || (next_visual_line && first_line_indented && previous_line_underfilled)
        {
            paragraphs.push(vec![line]);
        } else {
            paragraphs.last_mut().unwrap().push(line);
        }
    }
    paragraphs
}

fn split_at_legacy_indent_boundaries(mut lines: Vec<TextLine>, typical: f64) -> Vec<Vec<TextLine>> {
    let mut paragraphs = vec![vec![lines.remove(0)]];
    for line in lines {
        let previous = paragraphs.last().unwrap().last().unwrap();
        let gap = previous.baseline - line.baseline;
        let previous_width = previous.bounds.right - previous.bounds.left;
        let indented = line.bounds.left - previous.bounds.left > line.font_size * 1.2
            && previous_width < line.bounds.right - line.bounds.left;
        if gap > typical * 1.55 || indented {
            paragraphs.push(vec![line]);
        } else {
            paragraphs.last_mut().unwrap().push(line);
        }
    }
    paragraphs
}

fn infer_table_cell_bounds(lines: &mut [TextLine]) {
    let Some(table_bounds) = lines
        .iter()
        .flat_map(|line| &line.chars)
        .find_map(|character| {
            character
                .character
                .layout
                .filter(|layout| layout.label == LayoutLabel::Table)
                .map(|layout| layout.bounds)
        })
    else {
        return;
    };
    let original = lines.iter().map(|line| line.bounds).collect::<Vec<_>>();
    let mut rows = Vec::<Vec<usize>>::new();
    for index in 0..lines.len() {
        let belongs = rows.last().is_some_and(|row| {
            let anchor = row[0];
            (lines[anchor].baseline - lines[index].baseline).abs()
                <= lines[anchor].font_size.max(lines[index].font_size) * 0.35
        });
        if belongs {
            rows.last_mut().unwrap().push(index);
        } else {
            rows.push(vec![index]);
        }
    }
    for row in &mut rows {
        row.sort_by(|left, right| original[*left].left.total_cmp(&original[*right].left));
    }
    let row_text_bounds = rows
        .iter()
        .map(|row| {
            row[1..].iter().fold(original[row[0]], |bounds, index| {
                bounds.union(original[*index])
            })
        })
        .collect::<Vec<_>>();

    for (row_index, row) in rows.iter().enumerate() {
        let top = if row_index == 0 {
            table_bounds.top
        } else {
            (row_text_bounds[row_index - 1].bottom + row_text_bounds[row_index].top) / 2.0
        };
        let bottom = if row_index + 1 == rows.len() {
            table_bounds.bottom
        } else {
            (row_text_bounds[row_index].bottom + row_text_bounds[row_index + 1].top) / 2.0
        };
        for (column_index, &line_index) in row.iter().enumerate() {
            let left = if column_index == 0 {
                table_bounds.left
            } else {
                (original[row[column_index - 1]].right + original[line_index].left) / 2.0
            };
            let right = if column_index + 1 == row.len() {
                table_bounds.right
            } else {
                (original[line_index].right + original[row[column_index + 1]].left) / 2.0
            };
            if left.is_finite()
                && bottom.is_finite()
                && right.is_finite()
                && top.is_finite()
                && left < right
                && bottom < top
            {
                lines[line_index].bounds = Rect {
                    left,
                    bottom,
                    right,
                    top,
                };
            }
        }
    }
}

fn lines_bounds(lines: &[TextLine]) -> Rect {
    lines[1..]
        .iter()
        .fold(lines[0].bounds, |bounds, line| bounds.union(line.bounds))
}

fn attach_isolated_chars(drafts: &mut Vec<ParagraphDraft>, isolated: Vec<PositionedChar>) {
    for isolated in isolated {
        if drafts.is_empty() {
            let line = text_line(vec![isolated]);
            drafts.push(ParagraphDraft {
                model_order: None,
                apparatus: false,
                column_left: line.bounds.left,
                top: line.bounds.top,
                lines: vec![line],
            });
            continue;
        }
        let owner = drafts
            .iter()
            .enumerate()
            .flat_map(|(draft_index, draft)| {
                draft
                    .lines
                    .iter()
                    .enumerate()
                    .flat_map(move |(line_index, line)| {
                        line.chars
                            .iter()
                            .enumerate()
                            .map(move |(char_index, value)| {
                                (
                                    draft_index,
                                    line_index,
                                    char_index,
                                    value.walked_index.abs_diff(isolated.walked_index),
                                )
                            })
                    })
            })
            .min_by_key(|(_, _, _, distance)| *distance)
            .map(|(draft, line, character, _)| (draft, line, character))
            .unwrap();
        let line = &mut drafts[owner.0].lines[owner.1];
        let insert_at = if isolated.walked_index < line.chars[owner.2].walked_index {
            owner.2
        } else {
            owner.2 + 1
        };
        line.chars.insert(insert_at, isolated);
        let chars = std::mem::take(&mut line.chars);
        *line = text_line(chars);
    }
}

fn paragraph_from_lines(
    reading_order: usize,
    lines: Vec<TextLine>,
    walked: &[crate::walk::WalkedChar],
    weak_unicode_conflicts: &BTreeSet<usize>,
    page_content_objects: &BTreeSet<u32>,
) -> Paragraph {
    let bounds = lines_bounds(&lines);
    let first_line_indent = source_first_line_indent(&lines);
    let mut positioned = Vec::new();
    for line in lines {
        positioned.extend(line.chars);
    }
    for positioned in &mut positioned {
        if let Some(layout) = &mut positioned.character.layout
            && layout.label == LayoutLabel::Table
        {
            layout.bounds = bounds;
        }
    }
    let has_translatable_form_xobject_content = positioned.iter().any(|positioned| {
        let walked = &walked[positioned.walked_index];
        walked.visible
            && walked.text_transform == TextTransform::Upright
            && positioned
                .character
                .layout
                .is_some_and(|layout| layout.policy == TranslationPolicy::Translate)
            && !page_content_objects.contains(&walked.content_object.0)
    });
    let has_translatable_page_content = positioned.iter().any(|positioned| {
        let walked = &walked[positioned.walked_index];
        walked.visible
            && walked.text_transform == TextTransform::Upright
            && positioned
                .character
                .layout
                .is_some_and(|layout| layout.policy == TranslationPolicy::Translate)
            && page_content_objects.contains(&walked.content_object.0)
    });
    let preserved = paragraph_preserved_reason(
        positioned
            .iter()
            .map(|positioned| (positioned.walked_index, &walked[positioned.walked_index])),
        weak_unicode_conflicts,
        has_translatable_form_xobject_content && !has_translatable_page_content,
    );
    let mut chars = Vec::with_capacity(positioned.len());
    let mut previous_locatable = None;
    for mut positioned in positioned {
        let new_line = chars.last().is_some_and(|previous: &Char| {
            previous_locatable == Some(true)
                && positioned.locatable
                && (previous.baseline_origin.y - positioned.character.baseline_origin.y).abs()
                    > previous.font_size.max(positioned.character.font_size) * 0.35
        });
        let horizontal_gap = chars.last().map_or(0.0, |previous| {
            positioned.character.r#box.left - previous.r#box.right
        });
        let implicit_space = chars.last().is_some_and(|previous| {
            previous_locatable == Some(true)
                && positioned.locatable
                && previous.unicode.is_some_and(|value| !value.is_whitespace())
                && positioned
                    .character
                    .unicode
                    .is_some_and(|value| !value.is_whitespace())
                && previous.unicode != Some('-')
                && (new_line
                    || horizontal_gap
                        > previous.font_size.max(positioned.character.font_size) * 0.18)
        });
        positioned.character.implicit_space_before =
            implicit_space && !positioned.force_no_space_before;
        previous_locatable = Some(positioned.locatable);
        chars.push(positioned.character);
    }
    Paragraph {
        reading_order,
        bounds,
        first_line_indent,
        text: TextCarrier::Chars { chars },
        translated_text: None,
        translation_conservation: None,
        preserved,
    }
}

fn source_first_line_indent(lines: &[TextLine]) -> Option<f64> {
    if !lines
        .iter()
        .flat_map(|line| &line.chars)
        .all(|positioned| first_line_indent_character_is_prose(&positioned.character))
    {
        return None;
    }
    let first = line_baseline_start(lines.first()?)?;
    let mut continuation_starts = lines
        .iter()
        .skip(1)
        .filter_map(line_baseline_start)
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if continuation_starts.is_empty() {
        return None;
    }
    continuation_starts.sort_by(f64::total_cmp);
    let normal_start = continuation_starts[continuation_starts.len() / 2];
    let indent = first - normal_start;
    (indent.is_finite() && indent > 0.01).then_some(indent)
}

fn first_line_indent_character_is_prose(character: &Char) -> bool {
    character.visible
        && character.layout.is_some_and(|layout| {
            layout.source == LayoutSource::Model
                && layout.label == LayoutLabel::Text
                && layout.policy == TranslationPolicy::Translate
        })
}

fn line_baseline_start(line: &TextLine) -> Option<f64> {
    line.chars
        .iter()
        .find(|positioned| {
            positioned
                .character
                .unicode
                .is_some_and(|unicode| !unicode.is_whitespace())
        })
        .map(|positioned| positioned.character.baseline_origin.x)
}

pub fn styles_and_formulas(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    prepare_translations(document)?;
    Ok(())
}

fn term_extraction_document_text(document: &Document) -> String {
    document
        .prepared_translations
        .values()
        .filter(|prepared| !prepared.is_local_identity())
        .map(crate::translate::PreparedTranslation::request_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn extract_terms(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let document_text = term_extraction_document_text(document);
    let model_id = context.translator.model_id();
    let automatic = if context.config.auto_terms && model_id != "none" && !document_text.is_empty()
    {
        let cache = context
            .config
            .cache_path
            .as_deref()
            .map(crate::translate::cache::TranslationCache::open)
            .transpose()?;
        let cache_key = crate::translate::cache::TermExtractionCacheKey::new(
            &document_text,
            model_id,
            &context.config.target_language,
            crate::translate::TERMS_PROMPT_VERSION,
        );
        if let Some(glossary) = cache
            .as_ref()
            .map(|cache| cache.get_terms(&cache_key))
            .transpose()?
            .flatten()
        {
            glossary
        } else {
            let glossary =
                context
                    .translator
                    .extract_terms(&crate::translate::TermExtractionRequest {
                        document_text: &document_text,
                        target_language: &context.config.target_language,
                    })?;
            if let Some(cache) = &cache {
                cache.insert_terms(&cache_key, &glossary)?;
            }
            glossary
        }
    } else {
        crate::translate::Glossary::default()
    };
    document.glossary =
        crate::translate::Glossary::merged(automatic, &context.config.user_glossary);
    if let Some(path) = &context.config.dump_glossary {
        document.glossary.write_to_path(path)?;
    }
    Ok(())
}

pub fn translate(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    if document.prepared_translations.is_empty() {
        prepare_translations(document)?;
    }
    document.placeholder_violations.clear();
    document.suspicious_echoes.clear();
    let model_id = context.translator.model_id();
    if model_id == "none" {
        return translate_none(document, context);
    }
    if context.config.max_concurrency == 0 {
        return Err(MimusError::usage(
            UsageReason::InvalidArguments,
            "translation concurrency must be at least 1",
        ));
    }
    let cache = context
        .config
        .cache_path
        .as_deref()
        .map(crate::translate::cache::TranslationCache::open)
        .transpose()?;
    let glossary_fingerprint = document.glossary.fingerprint();
    let mut jobs = Vec::new();
    let mut prose_paragraph_count = 0;
    for (page_position, page) in document.il.pages.iter_mut().enumerate() {
        for (paragraph_index, paragraph) in page.paragraphs.iter_mut().enumerate() {
            paragraph.translation_conservation = None;
            if paragraph.preserved.is_some() {
                paragraph.translated_text = None;
                continue;
            }
            let key = (page.index, paragraph.reading_order);
            let prepared = document
                .prepared_translations
                .get(&key)
                .cloned()
                .ok_or_else(|| {
                    MimusError::internal(
                        InternalReason::InvariantViolation,
                        format!("Translate could not find prepared paragraph {key:?}"),
                    )
                })?;
            if prepared.is_local_identity() {
                paragraph.translated_text = Some(paragraph.source_text());
                paragraph.translation_conservation =
                    Some(prepared.identity_conservation_evidence());
                continue;
            }
            let prose_shaped = translation_request_is_prose_shaped(prepared.request_text());
            prose_paragraph_count += usize::from(prose_shaped);
            jobs.push(RemoteTranslationJob {
                page_position,
                page_index: page.index,
                paragraph_index,
                cache_key: crate::translate::cache::TranslationCacheKey::new(
                    prepared.request_text(),
                    model_id,
                    &context.config.target_language,
                    crate::translate::PARAGRAPH_PROMPT_VERSION,
                    &glossary_fingerprint,
                ),
                prepared,
                prose_shaped,
            });
        }
    }
    if jobs.is_empty() {
        return Ok(());
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(context.config.max_concurrency.min(jobs.len()))
        .thread_name(|index| format!("mimus-translate-{index}"))
        .build()
        .map_err(|_| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                "could not create translation worker pool",
            )
        })?;
    let executions = pool.install(|| {
        jobs.par_iter()
            .map(|job| {
                crate::translate::executor::execute(
                    context.translator,
                    cache.as_ref(),
                    context.config.sleeper.as_ref(),
                    crate::translate::executor::ExecutionRequest {
                        prepared: &job.prepared,
                        target_language: &context.config.target_language,
                        glossary: &document.glossary,
                        cache_key: &job.cache_key,
                    },
                )
            })
            .collect::<Vec<_>>()
    });
    let mut prose_identity_count = 0;
    for (job, execution) in jobs.into_iter().zip(executions) {
        if let Some(status) = execution.cache_status {
            context
                .events
                .emit(Event::new(EventKind::TranslationCache {
                    page_index: job.page_index,
                    paragraph_index: job.paragraph_index,
                    status,
                }))?;
        }
        for retry in execution.retries {
            document.diagnostics.push(Diagnostic::TranslationRetry {
                page_index: job.page_index,
                paragraph_index: job.paragraph_index,
                attempt: retry.attempt,
                delay_ms: retry.delay_ms,
                reason: retry.reason,
            });
        }
        for retry in execution.placeholder_retries {
            document.diagnostics.push(Diagnostic::PlaceholderRetry {
                page_index: job.page_index,
                paragraph_index: job.paragraph_index,
                attempt: retry.attempt,
                violation: retry.violation,
            });
        }
        for retry in execution.content_conservation_retries {
            document
                .diagnostics
                .push(Diagnostic::ContentConservationRetry {
                    page_index: job.page_index,
                    paragraph_index: job.paragraph_index,
                    attempt: retry.attempt,
                    missing_token_count: retry.missing_token_count,
                    missing_tokens: retry.missing_tokens,
                });
        }
        let paragraph = document
            .il
            .pages
            .get_mut(job.page_position)
            .and_then(|page| page.paragraphs.get_mut(job.paragraph_index))
            .ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "translation result no longer owns a paragraph",
                )
            })?;
        match execution.outcome {
            Ok(crate::translate::executor::TranslationOutcome::Translated {
                restored,
                conservation,
            }) => {
                paragraph.translated_text = Some(restored.plain_text());
                paragraph.translation_conservation = Some(conservation);
                document
                    .restored_translations
                    .insert((job.page_index, paragraph.reading_order), restored);
            }
            Ok(crate::translate::executor::TranslationOutcome::Identity {
                suspicious,
                conservation,
            }) => {
                paragraph.translated_text = Some(paragraph.source_text());
                paragraph.translation_conservation = Some(conservation);
                prose_identity_count += usize::from(job.prose_shaped);
                if suspicious {
                    document.diagnostics.push(Diagnostic::TranslationIdentity {
                        page_index: job.page_index,
                        paragraph_index: job.paragraph_index,
                        request_characters: job.prepared.request_text().chars().count(),
                    });
                    document
                        .suspicious_echoes
                        .insert((job.page_index, job.paragraph_index));
                    document.diagnostics.push(Diagnostic::SuspiciousEcho {
                        page_index: job.page_index,
                        paragraph_index: job.paragraph_index,
                        request_characters: job.prepared.request_text().chars().count(),
                    });
                }
            }
            Ok(crate::translate::executor::TranslationOutcome::PlaceholderViolation {
                violation,
                profile,
            }) => {
                preserve_placeholder_violation(
                    paragraph,
                    &mut document.diagnostics,
                    &mut document.placeholder_violations,
                    job.page_index,
                    job.paragraph_index,
                    violation,
                    profile,
                );
            }
            Ok(crate::translate::executor::TranslationOutcome::ContentConservationViolation {
                missing_token_count,
                missing_tokens,
            }) => {
                paragraph.translated_text = None;
                paragraph.preserved = Some(il::PreservedReason::ContentConservation);
                document
                    .diagnostics
                    .push(Diagnostic::ContentConservationViolation {
                        page_index: job.page_index,
                        paragraph_index: job.paragraph_index,
                        missing_token_count,
                        missing_tokens,
                    });
            }
            Err(error) if matches!(error.reason(), ErrorReason::Translation(_)) => {
                paragraph.translated_text = None;
                paragraph.preserved = Some(il::PreservedReason::TranslationFailure);
            }
            Err(error) => return Err(error),
        }
    }
    if prose_paragraph_count > 0 && prose_identity_count * 2 > prose_paragraph_count {
        document
            .diagnostics
            .push(Diagnostic::SuspiciousTranslationEchoRate {
                identity_count: prose_identity_count,
                prose_paragraph_count,
            });
    }
    Ok(())
}

struct RemoteTranslationJob {
    page_position: usize,
    page_index: usize,
    paragraph_index: usize,
    prepared: crate::translate::PreparedTranslation,
    cache_key: crate::translate::cache::TranslationCacheKey,
    prose_shaped: bool,
}

fn translate_none(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let page_content_objects = document
        .extracted_pages
        .iter()
        .map(|page| {
            page.content_streams
                .iter()
                .map(|stream| stream.object_id.0)
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    for page in &mut document.il.pages {
        let content_objects = page_content_objects.get(page.index).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                format!("Translate could not find extracted page {}", page.index),
            )
        })?;
        for paragraph in &mut page.paragraphs {
            paragraph.translation_conservation = None;
            if paragraph.preserved.is_some() {
                paragraph.translated_text = None;
                continue;
            }
            let prepared = document
                .prepared_translations
                .get(&(page.index, paragraph.reading_order))
                .ok_or_else(|| {
                    MimusError::internal(
                        InternalReason::InvariantViolation,
                        format!(
                            "Translate could not find prepared paragraph ({}, {})",
                            page.index, paragraph.reading_order
                        ),
                    )
                })?;
            if prepared.is_local_identity() {
                paragraph.translated_text = Some(paragraph.source_text());
                continue;
            }
            let chars = paragraph.chars();
            let mut translated = String::new();
            let mut start = 0;
            while start < chars.len() {
                let should_translate = character_is_translatable(&chars[start], content_objects);
                let mut end = start + 1;
                while end < chars.len()
                    && character_is_translatable(&chars[end], content_objects) == should_translate
                {
                    end += 1;
                }
                let source = request_text(&chars[start..end]);
                if should_translate && !source.is_empty() {
                    translated.push_str(&context.translator.translate(
                        &crate::translate::TranslationRequest {
                            text: &source,
                            target_language: &context.config.target_language,
                            glossary: &document.glossary,
                            placeholder_correction: None,
                            content_correction: None,
                        },
                    )?);
                } else {
                    translated.push_str(&source);
                }
                start = end;
            }
            paragraph.translated_text = Some(translated);
        }
    }
    Ok(())
}

fn translation_request_is_prose_shaped(request: &str) -> bool {
    let characters = request.chars().count();
    characters >= 40 && request.chars().filter(char::is_ascii_alphabetic).count() * 2 >= characters
}

fn preserve_placeholder_violation(
    paragraph: &mut Paragraph,
    diagnostics: &mut Diagnostics,
    placeholder_violations: &mut BTreeMap<(usize, usize), crate::translate::PlaceholderViolation>,
    page_index: usize,
    paragraph_index: usize,
    violation: crate::translate::PlaceholderViolation,
    profile: crate::translate::RedactedTranslationProfile,
) {
    paragraph.translated_text = None;
    paragraph.preserved = Some(il::PreservedReason::PlaceholderViolation);
    placeholder_violations.insert((page_index, paragraph_index), violation);
    diagnostics.push(Diagnostic::PlaceholderViolation {
        page_index,
        paragraph_index,
        violation,
    });
    diagnostics.push_debug(Diagnostic::TranslationFailureProfile {
        page_index,
        paragraph_index,
        response_bytes: profile.response_bytes,
        response_characters: profile.response_characters,
        token_count: profile.token_count,
        token_scan_valid: profile.token_scan_valid,
    });
}

fn prepare_translations(document: &mut Document) -> Result<()> {
    let page_content_objects = document
        .extracted_pages
        .iter()
        .map(|page| {
            page.content_streams
                .iter()
                .map(|stream| stream.object_id.0)
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    let page_vector_paths = document
        .extracted_pages
        .iter()
        .map(|page| page.vector_paths.clone())
        .collect::<Vec<_>>();
    let page_bold_source_spans = document
        .extracted_pages
        .iter()
        .map(|page| {
            page.walked_characters
                .iter()
                .filter(|character| character.is_bold)
                .map(|character| {
                    (
                        character.content_object.0,
                        character.byte_start,
                        character.byte_end,
                    )
                })
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    let mut prepared = BTreeMap::new();
    let mut math_diagnostics = Vec::new();
    for page in &mut document.il.pages {
        let content_objects = page_content_objects.get(page.index).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                format!(
                    "StylesAndFormulas could not find extracted page {}",
                    page.index
                ),
            )
        })?;
        let vector_paths = page_vector_paths.get(page.index).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                format!(
                    "StylesAndFormulas could not find vector paths for page {}",
                    page.index
                ),
            )
        })?;
        let bold_source_spans = page_bold_source_spans.get(page.index).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                format!(
                    "StylesAndFormulas could not find source styles for page {}",
                    page.index
                ),
            )
        })?;
        for (paragraph_index, paragraph) in page.paragraphs.iter_mut().enumerate() {
            if paragraph.preserved.is_some() {
                continue;
            }
            complete_model_formula_boundaries_with_paths(
                paragraph,
                content_objects,
                vector_paths,
                page.index,
                paragraph_index,
                &mut math_diagnostics,
            );
            mark_math_passthrough_units(
                paragraph,
                content_objects,
                page.index,
                paragraph_index,
                &mut math_diagnostics,
            );
            let chars = paragraph.chars();
            let mut parts = Vec::new();
            let mut start = 0;
            while start < chars.len() {
                let class = prepared_character_class_with_source_styles(
                    &chars[start],
                    content_objects,
                    bold_source_spans,
                );
                let mut end = start + 1;
                while end < chars.len()
                    && prepared_character_class_with_source_styles(
                        &chars[end],
                        content_objects,
                        bold_source_spans,
                    ) == class
                {
                    end += 1;
                }
                match class {
                    PreparedCharacterClass::Text { bold } => {
                        let text = request_text(&chars[start..end]);
                        if !text.is_empty() {
                            parts.push(crate::translate::PreparedPart::Text { text, bold });
                        }
                    }
                    PreparedCharacterClass::Formula => {
                        parts.push(crate::translate::PreparedPart::Formula);
                    }
                    PreparedCharacterClass::Passthrough => {}
                }
                start = end;
            }
            prepared.insert(
                (page.index, paragraph.reading_order),
                crate::translate::PreparedTranslation::new(parts),
            );
        }
    }
    document.prepared_translations = prepared;
    for diagnostic in math_diagnostics {
        document.diagnostics.push(diagnostic);
    }
    Ok(())
}

#[cfg(test)]
fn complete_model_formula_boundaries(
    paragraph: &mut Paragraph,
    content_objects: &BTreeSet<u32>,
    page_index: usize,
    paragraph_index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    complete_model_formula_boundaries_with_paths(
        paragraph,
        content_objects,
        &[],
        page_index,
        paragraph_index,
        diagnostics,
    );
}

fn complete_model_formula_boundaries_with_paths(
    paragraph: &mut Paragraph,
    content_objects: &BTreeSet<u32>,
    vector_paths: &[crate::walk::WalkedVectorPath],
    page_index: usize,
    paragraph_index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let TextCarrier::Chars { chars } = &mut paragraph.text;
    let anchors = chars
        .iter()
        .enumerate()
        .filter(|(_, character)| model_formula_character(character, content_objects))
        .map(|(index, character)| (index, character.layout.unwrap().reading_order))
        .collect::<Vec<_>>();
    if anchors.is_empty() {
        return;
    }
    let mut expanded = BTreeMap::<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>::new();
    expand_fraction_rule_numerators(chars, content_objects, vector_paths, &mut expanded);
    normalize_geometrically_attached_script_order(chars, content_objects, &mut expanded);
    let anchors = chars
        .iter()
        .enumerate()
        .filter(|(_, character)| model_formula_character(character, content_objects))
        .map(|(index, character)| (index, character.layout.unwrap().reading_order))
        .collect::<Vec<_>>();

    expand_script_runs(chars, content_objects, &anchors, &mut expanded);
    expand_contiguous_digit_runs(chars, content_objects, &anchors, &mut expanded);
    loop {
        let before = expanded.values().map(BTreeSet::len).sum::<usize>();
        expand_same_math_font_runs(chars, content_objects, &anchors, &mut expanded);
        expand_tightly_attached_suffixes(chars, content_objects, &anchors, &mut expanded);
        expand_balancing_delimiters(chars, content_objects, &anchors, &mut expanded);
        let after = expanded.values().map(BTreeSet::len).sum::<usize>();
        if after == before {
            break;
        }
    }
    normalize_fragmented_model_formula_order(chars, content_objects);

    for ((reading_order, evidence), indices) in expanded {
        diagnostics.push(Diagnostic::FormulaBoundaryExpanded {
            page_index,
            paragraph_index,
            reading_order,
            expanded_character_count: indices.len(),
            evidence,
        });
    }
}

fn expand_fraction_rule_numerators(
    chars: &mut [Char],
    content_objects: &BTreeSet<u32>,
    vector_paths: &[crate::walk::WalkedVectorPath],
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    let model_anchors = chars
        .iter()
        .enumerate()
        .filter(|(_, character)| model_formula_character(character, content_objects))
        .map(|(index, character)| {
            (
                index,
                character
                    .layout
                    .expect("model formula characters have layout")
                    .reading_order,
            )
        })
        .collect::<Vec<_>>();
    let mut additions = Vec::new();
    for (candidate_index, candidate) in chars.iter().enumerate() {
        if !formula_boundary_candidate(candidate, content_objects)
            || !candidate
                .unicode
                .is_some_and(|value| value.is_ascii_digit())
        {
            continue;
        }
        let mut reading_orders = BTreeSet::new();
        for path in vector_paths
            .iter()
            .filter(|path| fraction_rule_covers_numerator(path, candidate))
        {
            for (anchor_index, reading_order) in &model_anchors {
                if fraction_rule_covers_anchor(path, &chars[*anchor_index]) {
                    reading_orders.insert(*reading_order);
                }
            }
        }
        if reading_orders.len() == 1 {
            additions.push((candidate_index, *reading_orders.first().unwrap()));
        }
    }
    for (index, reading_order) in additions {
        mark_formula_extension(
            chars,
            index,
            reading_order,
            FormulaBoundaryEvidence::FractionRuleNumerator,
            expanded,
        );
        let layout = chars[index]
            .layout
            .as_mut()
            .expect("formula boundary candidates have layout");
        layout.reading_order = reading_order;
        chars[index].implicit_space_before = false;
    }
}

fn fraction_rule_covers_numerator(path: &crate::walk::WalkedVectorPath, candidate: &Char) -> bool {
    if path.content_object.0 != candidate.passthrough.content_object {
        return false;
    }
    let left = path.start.x.min(path.end.x);
    let right = path.start.x.max(path.end.x);
    let y = (path.start.y + path.end.y) / 2.0;
    let center_x = (candidate.r#box.left + candidate.r#box.right) / 2.0;
    let em = candidate.font_size;
    center_x >= left - em * 0.1
        && center_x <= right + em * 0.1
        && candidate.baseline_origin.y > y
        && candidate.r#box.bottom >= y - em * 0.25
        && candidate.passthrough.byte_end <= path.byte_start
        && path.byte_start - candidate.passthrough.byte_end <= 256
}

fn fraction_rule_covers_anchor(path: &crate::walk::WalkedVectorPath, anchor: &Char) -> bool {
    if path.content_object.0 != anchor.passthrough.content_object {
        return false;
    }
    let left = path.start.x.min(path.end.x);
    let right = path.start.x.max(path.end.x);
    let y = (path.start.y + path.end.y) / 2.0;
    let overlap = (right.min(anchor.r#box.right) - left.max(anchor.r#box.left)).max(0.0);
    let anchor_width = anchor.r#box.right - anchor.r#box.left;
    let byte_distance = if path.byte_end <= anchor.passthrough.byte_start {
        anchor.passthrough.byte_start - path.byte_end
    } else {
        path.byte_start.saturating_sub(anchor.passthrough.byte_end)
    };
    anchor.baseline_origin.y < y + anchor.font_size * 0.25
        && overlap >= anchor_width.min(right - left) * 0.5
        && byte_distance <= 256
}

fn expand_contiguous_digit_runs(
    chars: &mut [Char],
    content_objects: &BTreeSet<u32>,
    anchors: &[(usize, usize)],
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    let mut index = chars.len().saturating_sub(1);
    while index > 0 {
        if formula_character(&chars[index], content_objects)
            && chars[index]
                .unicode
                .is_some_and(|value| value.is_ascii_digit())
            && formula_boundary_candidate(&chars[index - 1], content_objects)
            && chars[index - 1]
                .unicode
                .is_some_and(|value| value.is_ascii_digit())
            && characters_are_attached(&chars[index - 1], &chars[index])
            && let Some(reading_order) = nearest_formula_reading_order(chars, anchors, index - 1)
        {
            mark_formula_extension(
                chars,
                index - 1,
                reading_order,
                FormulaBoundaryEvidence::ContiguousDigitRun,
                expanded,
            );
        }
        index -= 1;
    }

    let mut index = 1_usize;
    while index < chars.len() {
        if !formula_character(&chars[index - 1], content_objects)
            || !chars[index - 1]
                .unicode
                .is_some_and(|value| value.is_ascii_digit())
            || !formula_boundary_candidate(&chars[index], content_objects)
            || !chars[index]
                .unicode
                .is_some_and(|value| value.is_ascii_digit())
            || !characters_are_attached(&chars[index - 1], &chars[index])
        {
            index += 1;
            continue;
        }
        let Some(reading_order) = nearest_formula_reading_order(chars, anchors, index) else {
            index += 1;
            continue;
        };
        while index < chars.len()
            && formula_boundary_candidate(&chars[index], content_objects)
            && chars[index]
                .unicode
                .is_some_and(|value| value.is_ascii_digit())
            && characters_are_attached(&chars[index - 1], &chars[index])
        {
            mark_formula_extension(
                chars,
                index,
                reading_order,
                FormulaBoundaryEvidence::ContiguousDigitRun,
                expanded,
            );
            index += 1;
        }
    }
}

fn model_formula_character(character: &Char, content_objects: &BTreeSet<u32>) -> bool {
    character.visible
        && character.text_transform == TextTransform::Upright
        && character.layout.is_some_and(|layout| {
            layout.source == LayoutSource::Model && layout.label == LayoutLabel::InlineFormula
        })
        && content_objects.contains(&character.passthrough.content_object)
}

fn formula_boundary_candidate(character: &Char, content_objects: &BTreeSet<u32>) -> bool {
    character
        .unicode
        .is_some_and(|value| !value.is_whitespace())
        && character_is_translatable(character, content_objects)
        && character
            .layout
            .is_some_and(|layout| layout.source == LayoutSource::Model)
}

fn formula_character(character: &Char, content_objects: &BTreeSet<u32>) -> bool {
    character.visible
        && character.text_transform == TextTransform::Upright
        && character
            .layout
            .is_some_and(|layout| layout.label == LayoutLabel::InlineFormula)
        && content_objects.contains(&character.passthrough.content_object)
}

fn characters_are_attached(left: &Char, right: &Char) -> bool {
    if right.implicit_space_before {
        return false;
    }
    let em = left.font_size.max(right.font_size);
    let gap = right.r#box.left - left.r#box.right;
    gap >= -em * 0.25
        && gap <= em * 0.25
        && (left.baseline_origin.y - right.baseline_origin.y).abs() <= em * 0.35
}

fn formula_delimiter_is_attached(left: &Char, right: &Char) -> bool {
    if characters_are_attached(left, right) {
        return true;
    }
    let em = left.font_size.max(right.font_size);
    let gap = right.r#box.left - left.r#box.right;
    gap >= -em * 0.25
        && gap <= em * 0.25
        && rects_overlap_vertically(left.visual_bbox, right.visual_bbox)
}

fn mark_formula_extension(
    chars: &mut [Char],
    index: usize,
    reading_order: usize,
    evidence: FormulaBoundaryEvidence,
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    let layout = chars[index]
        .layout
        .as_mut()
        .expect("formula boundary candidates have model layout");
    layout.label = LayoutLabel::InlineFormula;
    layout.policy = TranslationPolicy::Passthrough;
    layout.reading_order = reading_order;
    expanded
        .entry((reading_order, evidence))
        .or_default()
        .insert(index);
}

fn normalize_fragmented_model_formula_order(
    chars: &mut Vec<Char>,
    content_objects: &BTreeSet<u32>,
) {
    let reading_orders = chars
        .iter()
        .filter(|character| {
            prepared_character_class(character, content_objects) == PreparedCharacterClass::Formula
        })
        .filter_map(|character| character.layout.map(|layout| layout.reading_order))
        .collect::<BTreeSet<_>>();
    for reading_order in reading_orders {
        let indices = chars
            .iter()
            .enumerate()
            .filter_map(|(index, character)| {
                character
                    .layout
                    .filter(|layout| {
                        layout.source == LayoutSource::Model
                            && layout.label == LayoutLabel::InlineFormula
                            && layout.policy == TranslationPolicy::Passthrough
                            && layout.reading_order == reading_order
                    })
                    .map(|_| index)
            })
            .collect::<Vec<_>>();
        if indices.len() < 2 || indices.windows(2).all(|pair| pair[1] == pair[0] + 1) {
            continue;
        }
        let formula = indices
            .iter()
            .map(|index| chars[*index].clone())
            .collect::<Vec<_>>();
        if !formula_characters_form_one_compound_unit(&formula) {
            continue;
        }
        let bounds = formula
            .iter()
            .flat_map(|character| [character.r#box, character.visual_bbox])
            .reduce(Rect::union)
            .expect("fragmented formula has characters");
        let em = formula
            .iter()
            .map(|character| character.font_size)
            .filter(|value| value.is_finite() && *value > 0.0)
            .fold(0.0_f64, f64::max);
        if em <= 0.0 || !rect_is_finite(bounds) {
            continue;
        }
        let remaining = chars
            .iter()
            .enumerate()
            .filter(|(index, _)| indices.binary_search(index).is_err())
            .map(|(_, character)| character.clone())
            .collect::<Vec<_>>();
        let candidates = remaining
            .windows(2)
            .enumerate()
            .filter_map(|(index, pair)| {
                let [left, right] = pair else {
                    return None;
                };
                if !matches!(
                    prepared_character_class(left, content_objects),
                    PreparedCharacterClass::Text { .. }
                ) || !matches!(
                    prepared_character_class(right, content_objects),
                    PreparedCharacterClass::Text { .. }
                ) || !rects_overlap_vertically(left.r#box, bounds)
                    || !rects_overlap_vertically(right.r#box, bounds)
                {
                    return None;
                }
                let left_gap = bounds.left - left.r#box.right;
                let right_gap = right.r#box.left - bounds.right;
                (left_gap >= -em * 0.25
                    && left_gap <= em * 1.5
                    && right_gap >= -em * 0.25
                    && right_gap <= em * 1.5)
                    .then_some(index + 1)
            })
            .collect::<Vec<_>>();
        let [insertion] = candidates.as_slice() else {
            continue;
        };
        let mut reordered = Vec::with_capacity(chars.len());
        reordered.extend_from_slice(&remaining[..*insertion]);
        reordered.extend(formula);
        reordered.extend_from_slice(&remaining[*insertion..]);
        if let Some(first) = reordered.first_mut() {
            first.implicit_space_before = false;
        }
        *chars = reordered;
    }
}

fn formula_characters_form_one_compound_unit(chars: &[Char]) -> bool {
    let Some(content_object) = chars
        .first()
        .map(|character| character.passthrough.content_object)
    else {
        return false;
    };
    if content_object == 0
        || chars
            .iter()
            .any(|character| character.passthrough.content_object != content_object)
    {
        return false;
    }
    let em = chars
        .iter()
        .map(|character| character.font_size)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(0.0_f64, f64::max);
    if em <= 0.0 {
        return false;
    }
    let mut connected = BTreeSet::from([0_usize]);
    loop {
        let before = connected.len();
        for (index, character) in chars.iter().enumerate() {
            if connected.contains(&index) {
                continue;
            }
            let candidate = character.r#box.union(character.visual_bbox);
            if connected.iter().any(|owner| {
                let owner = chars[*owner].r#box.union(chars[*owner].visual_bbox);
                let horizontal_gap = (candidate.left - owner.right)
                    .max(owner.left - candidate.right)
                    .max(0.0);
                let vertical_gap = (candidate.bottom - owner.top)
                    .max(owner.bottom - candidate.top)
                    .max(0.0);
                horizontal_gap <= em * 0.5 && vertical_gap <= em * 0.5
            }) {
                connected.insert(index);
            }
        }
        if connected.len() == before {
            break;
        }
    }
    connected.len() == chars.len()
}

fn expand_script_runs(
    chars: &mut [Char],
    content_objects: &BTreeSet<u32>,
    anchors: &[(usize, usize)],
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    for &(anchor_index, reading_order) in anchors {
        for direction in [-1_isize, 1] {
            let anchor = chars[anchor_index].clone();
            let mut previous = anchor_index;
            let mut next = anchor_index as isize + direction;
            while let Some(index) = usize::try_from(next)
                .ok()
                .filter(|index| *index < chars.len())
            {
                let candidate = &chars[index];
                if !formula_boundary_candidate(candidate, content_objects)
                    || candidate.font_size > anchor.font_size * 0.85
                    || (candidate.baseline_origin.y - anchor.baseline_origin.y).abs()
                        < anchor.font_size * 0.05
                {
                    break;
                }
                let attached = if direction < 0 {
                    characters_are_attached(candidate, &chars[previous])
                } else {
                    characters_are_attached(&chars[previous], candidate)
                };
                if !attached {
                    break;
                }
                mark_formula_extension(
                    chars,
                    index,
                    reading_order,
                    FormulaBoundaryEvidence::ScriptBaseline,
                    expanded,
                );
                previous = index;
                next += direction;
            }
        }
    }
}

fn normalize_geometrically_attached_script_order(
    chars: &mut [Char],
    content_objects: &BTreeSet<u32>,
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    normalize_geometrically_attached_signed_scripts(chars, content_objects, expanded);

    // A geometric script can be emitted before an intervening prose run. Move only
    // uniquely attached ASCII digits; the ordinary adjacent-script rule remains the
    // authority that promotes the reordered character into the formula unit.
    loop {
        let anchors = chars
            .iter()
            .enumerate()
            .filter(|(_, character)| model_formula_character(character, content_objects))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let next_move = chars
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                formula_boundary_candidate(candidate, content_objects)
                    && candidate
                        .unicode
                        .is_some_and(|value| value.is_ascii_digit())
            })
            .find_map(|(candidate_index, candidate)| {
                let matches = anchors
                    .iter()
                    .copied()
                    .filter(|anchor_index| {
                        geometrically_proves_script(&chars[*anchor_index], candidate)
                    })
                    .collect::<Vec<_>>();
                matches
                    .first()
                    .filter(|_| matches.len() == 1)
                    .and_then(|&anchor_index| {
                        let anchor = &chars[anchor_index];
                        let script_is_right =
                            candidate.r#box.left >= anchor.r#box.right - anchor.font_size * 0.25;
                        let already_adjacent = if script_is_right {
                            candidate_index == anchor_index + 1
                        } else {
                            candidate_index + 1 == anchor_index
                        };
                        (!already_adjacent).then_some((
                            candidate_index,
                            anchor_index,
                            script_is_right,
                        ))
                    })
            });
        let Some((candidate_index, anchor_index, script_is_right)) = next_move else {
            break;
        };
        let reading_order = chars[anchor_index]
            .layout
            .expect("model formula anchors have layout")
            .reading_order;
        mark_formula_extension(
            chars,
            candidate_index,
            reading_order,
            FormulaBoundaryEvidence::ScriptBaseline,
            expanded,
        );
        chars[candidate_index]
            .layout
            .as_mut()
            .expect("formula boundary candidates have model layout")
            .reading_order = reading_order;
        chars[candidate_index].implicit_space_before = false;
        if script_is_right {
            if candidate_index < anchor_index {
                chars[candidate_index..=anchor_index].rotate_left(1);
            } else {
                chars[anchor_index + 1..=candidate_index].rotate_right(1);
            }
        } else if candidate_index < anchor_index {
            chars[candidate_index..anchor_index].rotate_left(1);
        } else {
            chars[anchor_index..=candidate_index].rotate_right(1);
        }
    }
}

fn normalize_geometrically_attached_signed_scripts(
    chars: &mut [Char],
    content_objects: &BTreeSet<u32>,
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    loop {
        let matches = chars
            .iter()
            .enumerate()
            .filter(|(_, anchor)| {
                model_formula_character(anchor, content_objects)
                    && anchor.unicode.is_some_and(|value| value.is_ascii_digit())
            })
            .filter_map(|(anchor_index, anchor)| {
                let sign_index = anchor_index.checked_sub(1)?;
                let sign = &chars[sign_index];
                if !formula_boundary_candidate(sign, content_objects)
                    || !sign
                        .unicode
                        .is_some_and(|value| matches!(value, '+' | '-' | '−'))
                    || (sign.font_size - anchor.font_size).abs() > anchor.font_size * 0.1
                    || (sign.baseline_origin.y - anchor.baseline_origin.y).abs()
                        > anchor.font_size * 0.1
                    || !characters_are_attached(sign, anchor)
                {
                    return None;
                }
                let bases = chars
                    .iter()
                    .enumerate()
                    .filter(|(base_index, base)| {
                        *base_index != sign_index
                            && formula_boundary_candidate(base, content_objects)
                            && base.unicode.is_some_and(|value| value.is_ascii_digit())
                            && geometrically_proves_script(base, sign)
                    })
                    .map(|(base_index, _)| base_index)
                    .collect::<Vec<_>>();
                bases
                    .first()
                    .filter(|_| bases.len() == 1)
                    .map(|&base_index| (sign_index, anchor_index, base_index))
            })
            .collect::<Vec<_>>();
        let Some(&(sign_index, anchor_index, base_index)) = matches.first() else {
            break;
        };
        if matches.len() != 1 {
            break;
        }
        let reading_order = chars[anchor_index]
            .layout
            .expect("model formula anchors have layout")
            .reading_order;
        for index in [sign_index, base_index] {
            mark_formula_extension(
                chars,
                index,
                reading_order,
                FormulaBoundaryEvidence::ScriptBaseline,
                expanded,
            );
            chars[index]
                .layout
                .as_mut()
                .expect("formula boundary candidates have model layout")
                .reading_order = reading_order;
            chars[index].implicit_space_before = false;
        }
        if anchor_index < base_index {
            chars[sign_index..=base_index].rotate_left(2);
        } else if base_index + 1 != sign_index {
            chars[base_index + 1..=anchor_index].rotate_right(2);
        }
    }
}

fn geometrically_proves_script(anchor: &Char, candidate: &Char) -> bool {
    let em = anchor.font_size;
    if !em.is_finite()
        || em <= 0.0
        || !candidate.font_size.is_finite()
        || candidate.font_size < em * 0.4
        || candidate.font_size > em * 0.85
    {
        return false;
    }
    let baseline_delta = (candidate.baseline_origin.y - anchor.baseline_origin.y).abs();
    if baseline_delta < em * 0.05 || baseline_delta > em * 0.75 {
        return false;
    }
    let right_gap = candidate.r#box.left - anchor.r#box.right;
    let left_gap = anchor.r#box.left - candidate.r#box.right;
    (right_gap >= -em * 0.25 && right_gap <= em * 0.25)
        || (left_gap >= -em * 0.25 && left_gap <= em * 0.25)
}

fn nearest_formula_reading_order(
    chars: &[Char],
    anchors: &[(usize, usize)],
    index: usize,
) -> Option<usize> {
    anchors
        .iter()
        .filter(|(anchor_index, _)| {
            let formula_path = match anchor_index.cmp(&index) {
                std::cmp::Ordering::Less => &chars[*anchor_index..index],
                std::cmp::Ordering::Greater => &chars[index + 1..=*anchor_index],
                std::cmp::Ordering::Equal => return false,
            };
            formula_path.iter().all(|character| {
                character
                    .layout
                    .is_some_and(|layout| layout.label == LayoutLabel::InlineFormula)
            })
        })
        .min_by_key(|(anchor_index, _)| anchor_index.abs_diff(index))
        .map(|(_, reading_order)| *reading_order)
}

fn unicode_proves_math_font(value: char) -> bool {
    ('\u{1d400}'..='\u{1d7ff}').contains(&value)
}

fn expand_same_math_font_runs(
    chars: &mut [Char],
    content_objects: &BTreeSet<u32>,
    anchors: &[(usize, usize)],
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    loop {
        let mut additions = Vec::new();
        for index in 0..chars.len() {
            let candidate = &chars[index];
            if !formula_boundary_candidate(candidate, content_objects)
                || !candidate.unicode.is_some_and(char::is_alphanumeric)
                || !anchors
                    .iter()
                    .filter(|(anchor, _)| {
                        chars[*anchor].font == candidate.font
                            && chars[*anchor].unicode.is_some_and(unicode_proves_math_font)
                    })
                    .any(|anchor| {
                        nearest_formula_reading_order(chars, std::slice::from_ref(anchor), index)
                            .is_some()
                    })
            {
                continue;
            }
            let attached = index
                .checked_sub(1)
                .filter(|left| formula_character(&chars[*left], content_objects))
                .is_some_and(|left| characters_are_attached(&chars[left], candidate))
                || chars
                    .get(index + 1)
                    .filter(|right| formula_character(right, content_objects))
                    .is_some_and(|right| characters_are_attached(candidate, right));
            if attached
                && let Some(reading_order) = nearest_formula_reading_order(chars, anchors, index)
            {
                additions.push((index, reading_order));
            }
        }
        if additions.is_empty() {
            break;
        }
        for (index, reading_order) in additions {
            mark_formula_extension(
                chars,
                index,
                reading_order,
                FormulaBoundaryEvidence::SameMathFontRun,
                expanded,
            );
        }
    }
}

fn tightly_attached_math_suffix(value: char) -> bool {
    matches!(value, '\'' | '′' | '″' | '‴' | '!' | '%' | '°') || ('⁰'..='₟').contains(&value)
}

fn expand_tightly_attached_suffixes(
    chars: &mut [Char],
    content_objects: &BTreeSet<u32>,
    anchors: &[(usize, usize)],
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    for index in 1..chars.len() {
        if !formula_boundary_candidate(&chars[index], content_objects)
            || !chars[index]
                .unicode
                .is_some_and(tightly_attached_math_suffix)
            || !formula_character(&chars[index - 1], content_objects)
            || !characters_are_attached(&chars[index - 1], &chars[index])
        {
            continue;
        }
        let Some(reading_order) = nearest_formula_reading_order(chars, anchors, index) else {
            continue;
        };
        mark_formula_extension(
            chars,
            index,
            reading_order,
            FormulaBoundaryEvidence::TightlyAttachedSuffix,
            expanded,
        );
    }
}

fn matching_delimiter(value: char) -> Option<char> {
    match value {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '⟨' => Some('⟩'),
        _ => None,
    }
}

fn opening_delimiter(value: char) -> Option<char> {
    match value {
        ')' => Some('('),
        ']' => Some('['),
        '}' => Some('{'),
        '⟩' => Some('⟨'),
        _ => None,
    }
}

fn expand_balancing_delimiters(
    chars: &mut [Char],
    content_objects: &BTreeSet<u32>,
    anchors: &[(usize, usize)],
    expanded: &mut BTreeMap<(usize, FormulaBoundaryEvidence), BTreeSet<usize>>,
) {
    let mut start = 0;
    while start < chars.len() {
        if !formula_character(&chars[start], content_objects) {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < chars.len() && formula_character(&chars[end], content_objects) {
            end += 1;
        }
        let mut stack = Vec::new();
        let mut unmatched_close = None;
        for character in &chars[start..end] {
            let Some(value) = character.unicode else {
                continue;
            };
            if let Some(close) = matching_delimiter(value) {
                stack.push((value, close));
            } else if let Some(open) = opening_delimiter(value) {
                if stack.last().is_some_and(|(expected, _)| *expected == open) {
                    stack.pop();
                } else {
                    unmatched_close.get_or_insert(open);
                }
            }
        }

        if let Some((_, expected)) = stack.last().copied()
            && end < chars.len()
            && formula_boundary_candidate(&chars[end], content_objects)
            && chars[end].unicode == Some(expected)
            && formula_delimiter_is_attached(&chars[end - 1], &chars[end])
        {
            let Some(reading_order) = nearest_formula_reading_order(chars, anchors, end) else {
                start = end;
                continue;
            };
            mark_formula_extension(
                chars,
                end,
                reading_order,
                FormulaBoundaryEvidence::DelimiterCompletion,
                expanded,
            );
        } else if let Some(expected) = unmatched_close
            && start > 0
            && formula_boundary_candidate(&chars[start - 1], content_objects)
            && chars[start - 1].unicode == Some(expected)
            && formula_delimiter_is_attached(&chars[start - 1], &chars[start])
        {
            let index = start - 1;
            let Some(reading_order) = nearest_formula_reading_order(chars, anchors, index) else {
                start = end;
                continue;
            };
            mark_formula_extension(
                chars,
                index,
                reading_order,
                FormulaBoundaryEvidence::DelimiterCompletion,
                expanded,
            );
        }
        start = end;
    }
}

fn mark_math_passthrough_units(
    paragraph: &mut Paragraph,
    content_objects: &BTreeSet<u32>,
    page_index: usize,
    paragraph_index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let TextCarrier::Chars { chars } = &mut paragraph.text;
    let mut math_units = Vec::new();
    let model_math = chars
        .first()
        .and_then(|character| character.layout)
        .filter(|layout| layout.source == LayoutSource::Model && layout.label == LayoutLabel::Text)
        .filter(|_| {
            chars.iter().all(|character| {
                character.layout.is_some_and(|layout| {
                    layout.source == LayoutSource::Model && layout.label == LayoutLabel::Text
                })
            })
        })
        .map(|layout| (layout, request_text(chars)))
        .filter(|(_, source)| model_math_shape_is_passthrough(source))
        .filter(|_| {
            chars
                .iter()
                .any(|character| character_is_translatable(character, content_objects))
        });
    if let Some((layout, source)) = model_math {
        math_units.push((layout.reading_order, source.trim().chars().count()));
    } else {
        let mut start = 0;
        while start < chars.len() {
            let layout = chars[start].layout;
            let mut end = start + 1;
            while end < chars.len() && chars[end].layout == layout {
                end += 1;
            }
            let Some(layout) = layout else {
                start = end;
                continue;
            };
            if layout.source != LayoutSource::FallbackLine {
                start = end;
                continue;
            }
            let source = request_text(&chars[start..end]);
            if math_shape_is_passthrough(&source)
                && chars[start..end]
                    .iter()
                    .any(|character| character_is_translatable(character, content_objects))
            {
                math_units.push((layout.reading_order, source.trim().chars().count()));
            }
            start = end;
        }
    }
    if math_units.is_empty() {
        return;
    }
    for character in chars {
        if character_is_translatable(character, content_objects) {
            character.layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        }
    }
    for (reading_order, source_characters) in math_units {
        diagnostics.push(Diagnostic::MathPassthrough {
            page_index,
            paragraph_index,
            reading_order,
            source_characters,
        });
    }
}

fn math_shape_is_passthrough(source: &str) -> bool {
    let text = source.trim();
    if text.is_empty() {
        return false;
    }
    if text
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .is_some_and(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        })
    {
        return true;
    }

    let characters = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<Vec<_>>();
    if characters.len() == 1 {
        let character = characters[0];
        if matches!(
            character,
            'Q' | 'K'
                | 'V'
                | 'W'
                | 'X'
                | 'Y'
                | 'Z'
                | 'q'
                | 'k'
                | 'v'
                | 'w'
                | 'x'
                | 'y'
                | 'z'
                | 'i'
                | 'j'
        ) || ('Ͱ'..='Ͽ').contains(&character)
        {
            return true;
        }
    }

    let has_operand = characters
        .iter()
        .any(|character| character.is_alphanumeric());
    let has_strong_operator = characters
        .iter()
        .any(|character| math_shape_has_strong_operator(*character));
    if has_operand && has_strong_operator {
        return true;
    }

    let has_script = characters
        .iter()
        .any(|character| matches!(character, '²' | '³' | '¹') || ('⁰'..='₟').contains(character));
    if has_operand && has_script {
        return true;
    }

    let syntax_characters = characters
        .iter()
        .filter(|character| {
            matches!(
                character,
                '(' | ')' | '[' | ']' | '{' | '}' | ',' | '+' | '-' | '*' | '/' | '^' | '_'
            )
        })
        .count();
    has_operand && syntax_characters >= 3 && syntax_characters.saturating_mul(5) >= characters.len()
}

fn model_math_shape_is_passthrough(source: &str) -> bool {
    math_shape_is_passthrough(source)
        && source.chars().any(math_shape_has_strong_operator)
        && source
            .split_whitespace()
            .filter(|token| token.chars().any(char::is_alphanumeric))
            .count()
            <= 2
}

fn math_shape_has_strong_operator(character: char) -> bool {
    matches!(
        character,
        '=' | '×'
            | '÷'
            | '±'
            | '∓'
            | '∑'
            | '∏'
            | '∫'
            | '√'
            | '∈'
            | '∉'
            | '≤'
            | '≥'
            | '≠'
            | '≈'
            | '∞'
            | '∂'
            | '∇'
            | '⊗'
            | '⋅'
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreparedCharacterClass {
    Text { bold: bool },
    Formula,
    Passthrough,
}

fn prepared_character_class(
    character: &Char,
    content_objects: &BTreeSet<u32>,
) -> PreparedCharacterClass {
    prepared_character_class_with_source_styles(character, content_objects, &BTreeSet::new())
}

fn prepared_character_class_with_source_styles(
    character: &Char,
    content_objects: &BTreeSet<u32>,
    bold_source_spans: &BTreeSet<(u32, usize, usize)>,
) -> PreparedCharacterClass {
    if character.visible
        && character.text_transform == TextTransform::Upright
        && character
            .layout
            .is_some_and(|layout| layout.label == LayoutLabel::InlineFormula)
        && content_objects.contains(&character.passthrough.content_object)
    {
        return PreparedCharacterClass::Formula;
    }
    if character_is_translatable(character, content_objects) {
        let bold = character.layout.is_some_and(|layout| {
            matches!(
                layout.label,
                LayoutLabel::DocTitle | LayoutLabel::ParagraphTitle
            )
        }) || character
            .font
            .resource_name
            .to_ascii_lowercase()
            .contains("bold")
            || bold_source_spans.contains(&(
                character.passthrough.content_object,
                character.passthrough.byte_start,
                character.passthrough.byte_end,
            ));
        PreparedCharacterClass::Text { bold }
    } else {
        PreparedCharacterClass::Passthrough
    }
}

fn request_text(chars: &[Char]) -> String {
    let mut output = String::new();
    for character in chars {
        let Some(unicode) = character.unicode else {
            continue;
        };
        if character.implicit_space_before
            && !output.ends_with(char::is_whitespace)
            && !unicode.is_whitespace()
        {
            output.push(' ');
        }
        output.push(unicode);
    }
    output
}

fn character_is_translatable(character: &Char, content_objects: &BTreeSet<u32>) -> bool {
    character.unicode.is_some()
        && character.visible
        && character.text_transform == TextTransform::Upright
        && character
            .layout
            .is_some_and(|layout| layout.policy == TranslationPolicy::Translate)
        && content_objects.contains(&character.passthrough.content_object)
}

pub fn typeset(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let needs_output_fonts = document.il.pages.iter().any(|page| {
        page.paragraphs.iter().any(|paragraph| {
            paragraph.preserved.is_none()
                && paragraph
                    .translated_text
                    .as_deref()
                    .is_some_and(|translated| translated != paragraph.source_text())
        })
    });
    if needs_output_fonts && context.config.output_fonts.is_none() {
        return Err(MimusError::asset(
            AssetReason::OutputFontUnavailable,
            "translated text requires resolved output fonts",
        ));
    }
    let mut rewrites = Vec::with_capacity(document.il.pages.len());
    let mut preserved = Vec::new();
    let mut publication_ink = Vec::new();
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
        let page_bounds = extracted.frame.map(|frame| frame.crop_box).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                format!("Typeset page {} has no resolved page frame", page.index),
            )
        })?;
        let has_preserved = page
            .paragraphs
            .iter()
            .any(|paragraph| paragraph.preserved.is_some());
        let processable_output_is_identity = page.paragraphs.iter().all(|paragraph| {
            paragraph.preserved.is_some()
                || paragraph.translated_text.as_deref() == Some(paragraph.source_text().as_str())
        });
        if has_preserved && processable_output_is_identity {
            // Avoid an incremental update when the only changed units are identity output and
            // another paragraph on the page must remain byte-exact (for example a shared Form).
            continue;
        }
        let streams = extracted
            .content_streams
            .iter()
            .map(|stream| (stream.object_id, stream.decoded.as_slice()))
            .collect::<BTreeMap<_, _>>();
        let content_objects = streams.keys().copied().collect::<BTreeSet<_>>();
        let content_transforms = extracted
            .walked_characters
            .iter()
            .map(|character| {
                (
                    (
                        character.content_object,
                        character.byte_start,
                        character.byte_end,
                    ),
                    character.content_transform,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut text_show_states = BTreeMap::new();
        for character in &extracted.walked_characters {
            text_show_states
                .entry((
                    character.content_object,
                    character.byte_start,
                    character.byte_end,
                ))
                .or_insert(TextShowState {
                    line_matrix: character.text_line_matrix,
                    matrix_after_show: character.text_matrix_after_show,
                    font_size: character.font_size,
                    horizontal_scale: character.horizontal_scale,
                });
        }
        let mut replacements = BTreeMap::<(lopdf::ObjectId, usize, usize), Vec<u8>>::new();
        let mut reused_fonts = BTreeSet::new();
        let mut planned_paragraphs = Vec::<(usize, Vec<TypesetPlan>)>::new();
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
            let translated = paragraph.translated_text.as_deref().ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "an unpreserved paragraph has no translated text",
                )
            })?;
            let chars = paragraph.chars();
            if chars.is_empty() {
                return Err(MimusError::input(
                    InputReason::UnsupportedPdf,
                    "cannot typeset an empty line",
                ));
            }
            if translated == source {
                continue;
            }

            let output_fonts = context
                .config
                .output_fonts
                .as_ref()
                .expect("non-identity output font requirement was checked");
            let restored = document
                .restored_translations
                .get(&(page.index, paragraph.reading_order));
            let Some(owned_underlines) =
                uniquely_owned_text_underlines(paragraph, extracted, &content_objects)
            else {
                preserved.push((
                    page.index,
                    paragraph.reading_order,
                    il::PreservedReason::TypesetProtocol,
                ));
                continue;
            };
            let underline_spans = owned_underlines
                .iter()
                .map(|underline| underline.replay.span)
                .collect::<BTreeSet<_>>();
            let obstacles =
                paragraph_typeset_obstacles(page, extracted, paragraph, true, &underline_spans);
            let relocation_obstacles =
                paragraph_typeset_obstacles(page, extracted, paragraph, false, &underline_spans);
            let geometry = ParagraphPlanningGeometry {
                extracted,
                content_objects: &content_objects,
                page_bounds,
                obstacles: &obstacles,
                relocation_obstacles: &relocation_obstacles,
            };
            match plan_paragraph(paragraph, translated, restored, output_fonts, &geometry) {
                Ok(mut paragraph_plans) => {
                    let Some(typeset_container) = paragraph_typeset_container(paragraph) else {
                        preserved.push((
                            page.index,
                            paragraph.reading_order,
                            il::PreservedReason::TypesetProtocol,
                        ));
                        continue;
                    };
                    if attach_text_underlines_to_plans(
                        &mut paragraph_plans,
                        &owned_underlines,
                        page_bounds,
                        typeset_container,
                        &obstacles,
                    )
                    .is_none()
                    {
                        preserved.push((
                            page.index,
                            paragraph.reading_order,
                            il::PreservedReason::TypesetProtocol,
                        ));
                        continue;
                    }
                    if paragraph_plans_leave_orphan_source_ink(
                        page,
                        paragraph,
                        extracted,
                        &content_objects,
                        &paragraph_plans,
                    )? {
                        preserved.push((
                            page.index,
                            paragraph.reading_order,
                            il::PreservedReason::TypesetProtocol,
                        ));
                    } else {
                        planned_paragraphs.push((paragraph.reading_order, paragraph_plans));
                    }
                }
                Err(TypesetPlanError::Preserved(reason)) => {
                    if reason == il::PreservedReason::TypesetOverflow {
                        document
                            .diagnostics
                            .push(typeset_overflow_detail(page.index, paragraph, &obstacles));
                    }
                    preserved.push((page.index, paragraph.reading_order, reason));
                }
                Err(TypesetPlanError::MissingGlyphs {
                    missing_characters,
                    primary_font,
                    fallback_font,
                }) => {
                    document
                        .diagnostics
                        .push(Diagnostic::UnsupportedOutputGlyph {
                            page_index: page.index,
                            reading_order: paragraph.reading_order,
                            missing_characters,
                            font_source: primary_font.source.clone(),
                            font_sha256: primary_font.sha256.clone(),
                            fallback_font_source: fallback_font.source.clone(),
                            fallback_font_sha256: fallback_font.sha256.clone(),
                        });
                    preserved.push((
                        page.index,
                        paragraph.reading_order,
                        il::PreservedReason::UnsupportedFont,
                    ));
                }
            }
        }

        let translated_spans = planned_paragraphs
            .iter()
            .flat_map(|(_, plans)| plans)
            .flat_map(plan_modified_spans)
            .collect::<BTreeSet<_>>();
        let mut blocked_spans = BTreeSet::new();
        for paragraph in &page.paragraphs {
            if paragraph.preserved.is_none() {
                continue;
            }
            for character in paragraph.chars() {
                let Some(content_object) = unique_page_content(character, &content_objects)? else {
                    continue;
                };
                blocked_spans.insert(span_key(character, content_object));
            }
        }
        for paragraph in &page.paragraphs {
            if paragraph.preserved.is_some() {
                continue;
            }
            let source = paragraph.source_text();
            let translated = paragraph.translated_text.as_deref().ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "an unpreserved paragraph has no translated text",
                )
            })?;
            if translated != source {
                continue;
            }
            let mut identity_spans = Vec::new();
            for character in paragraph.chars() {
                if character.layout.is_some_and(|layout| {
                    layout.label == LayoutLabel::Table
                        && layout.policy == TranslationPolicy::Passthrough
                }) {
                    continue;
                }
                let Some(content_object) = unique_page_content(character, &content_objects)? else {
                    continue;
                };
                if !character.visible || character.text_transform != TextTransform::Upright {
                    continue;
                }
                identity_spans.push(span_key(character, content_object));
            }
            identity_spans.sort_unstable();
            identity_spans.dedup();
            if identity_spans
                .iter()
                .any(|span| translated_spans.contains(span))
            {
                let output_fonts = context
                    .config
                    .output_fonts
                    .as_ref()
                    .expect("non-identity output font requirement was checked");
                let restored = document
                    .restored_translations
                    .get(&(page.index, paragraph.reading_order));
                let obstacles =
                    paragraph_typeset_obstacles(page, extracted, paragraph, true, &BTreeSet::new());
                let relocation_obstacles = paragraph_typeset_obstacles(
                    page,
                    extracted,
                    paragraph,
                    false,
                    &BTreeSet::new(),
                );
                let geometry = ParagraphPlanningGeometry {
                    extracted,
                    content_objects: &content_objects,
                    page_bounds,
                    obstacles: &obstacles,
                    relocation_obstacles: &relocation_obstacles,
                };
                match plan_paragraph(paragraph, translated, restored, output_fonts, &geometry) {
                    Ok(paragraph_plans) => {
                        planned_paragraphs.push((paragraph.reading_order, paragraph_plans));
                    }
                    Err(_) => match plan_shared_number_identity(
                        paragraph,
                        translated,
                        &content_objects,
                        output_fonts,
                        page_bounds,
                        &obstacles,
                    ) {
                        Ok(paragraph_plan) => {
                            planned_paragraphs
                                .push((paragraph.reading_order, vec![paragraph_plan]));
                        }
                        Err(_) => {
                            blocked_spans.extend(identity_spans.iter().copied());
                        }
                    },
                }
                continue;
            }
            for character in paragraph.chars() {
                let Some(content_object) = unique_page_content(character, &content_objects)? else {
                    continue;
                };
                let key = span_key(character, content_object);
                if !identity_spans.contains(&key) {
                    continue;
                }
                let bytes = streams[&content_object].get(key.1..key.2).ok_or_else(|| {
                    span_out_of_bounds(content_object, key.1, key.2, streams[&content_object].len())
                })?;
                replacements.entry(key).or_insert_with(|| bytes.to_vec());
                reused_fonts.insert(character.font.clone());
            }
        }
        if !blocked_spans.is_empty() {
            loop {
                let previous_len = blocked_spans.len();
                for (_, plans) in &planned_paragraphs {
                    if plans
                        .iter()
                        .flat_map(plan_modified_spans)
                        .any(|span| blocked_spans.contains(&span))
                    {
                        blocked_spans.extend(plans.iter().flat_map(plan_modified_spans));
                    }
                }
                if blocked_spans.len() == previous_len {
                    break;
                }
            }
            planned_paragraphs.retain(|(reading_order, plans)| {
                let blocked = plans
                    .iter()
                    .flat_map(plan_modified_spans)
                    .any(|span| blocked_spans.contains(&span));
                if !blocked {
                    return true;
                }
                let paragraph = page
                    .paragraphs
                    .iter()
                    .find(|paragraph| paragraph.reading_order == *reading_order)
                    .expect("planned paragraph belongs to its page");
                if paragraph.translated_text.as_deref() != Some(paragraph.source_text().as_str()) {
                    preserved.push((
                        page.index,
                        *reading_order,
                        il::PreservedReason::TypesetProtocol,
                    ));
                }
                false
            });
        }
        for (reading_order, plans) in &planned_paragraphs {
            for expansion in plans.iter().filter_map(|plan| plan.single_line_expansion) {
                document
                    .diagnostics
                    .push(Diagnostic::SingleLineBoundsExpanded {
                        page_index: page.index,
                        reading_order: *reading_order,
                        overflow_top_pt: expansion.top_pt,
                        overflow_bottom_pt: expansion.bottom_pt,
                    });
            }
            for expansion in plans.iter().filter_map(|plan| plan.multi_line_expansion) {
                document
                    .diagnostics
                    .push(Diagnostic::MultiLineBoundsExpanded {
                        page_index: page.index,
                        reading_order: *reading_order,
                        overflow_top_pt: expansion.top_pt,
                        overflow_bottom_pt: expansion.bottom_pt,
                    });
            }
        }
        planned_paragraphs.sort_by_key(|(reading_order, _)| *reading_order);
        let configured_fonts = context.config.output_fonts.as_ref();
        if !planned_paragraphs.is_empty() {
            let configured_fonts = configured_fonts.ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "translated text has no resolved output fonts",
                )
            })?;
            let probe_fonts = build_typeset_fonts(
                planned_paragraphs
                    .iter()
                    .flat_map(|(_, plans)| plans.iter()),
                configured_fonts,
            )?;
            let incompatible = incompatible_plan_component_indices(
                &planned_paragraphs,
                &probe_fonts,
                &streams,
                &content_transforms,
                &text_show_states,
                &replacements,
            )?;
            planned_paragraphs = planned_paragraphs
                .into_iter()
                .enumerate()
                .filter_map(|(index, planned)| {
                    if !incompatible.contains(&index) {
                        return Some(planned);
                    }
                    let (reading_order, _) = &planned;
                    let paragraph = page
                        .paragraphs
                        .iter()
                        .find(|paragraph| paragraph.reading_order == *reading_order)
                        .expect("planned paragraph belongs to its page");
                    if paragraph.translated_text.as_deref()
                        != Some(paragraph.source_text().as_str())
                    {
                        preserved.push((
                            page.index,
                            *reading_order,
                            il::PreservedReason::TypesetProtocol,
                        ));
                    }
                    None
                })
                .collect();
        }
        let mut output_fonts = BTreeMap::new();
        let mut typeset_characters = Vec::new();
        if !planned_paragraphs.is_empty() {
            let configured_fonts = configured_fonts.ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "translated text has no resolved output fonts",
                )
            })?;
            output_fonts = build_typeset_fonts(
                planned_paragraphs
                    .iter()
                    .flat_map(|(_, plans)| plans.iter()),
                configured_fonts,
            )?;
            let incompatible = incompatible_final_ink_indices(
                &planned_paragraphs,
                page,
                extracted,
                page_bounds,
                &output_fonts,
            )?;
            if !incompatible.is_empty() {
                planned_paragraphs = planned_paragraphs
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, planned)| {
                        if !incompatible.contains(&index) {
                            return Some(planned);
                        }
                        let (reading_order, _) = &planned;
                        let paragraph = page
                            .paragraphs
                            .iter()
                            .find(|paragraph| paragraph.reading_order == *reading_order)
                            .expect("planned paragraph belongs to its page");
                        if paragraph.translated_text.as_deref()
                            != Some(paragraph.source_text().as_str())
                        {
                            preserved.push((
                                page.index,
                                *reading_order,
                                il::PreservedReason::TypesetOverflow,
                            ));
                        }
                        None
                    })
                    .collect();
                output_fonts = if planned_paragraphs.is_empty() {
                    BTreeMap::new()
                } else {
                    build_typeset_fonts(
                        planned_paragraphs
                            .iter()
                            .flat_map(|(_, plans)| plans.iter()),
                        configured_fonts,
                    )?
                };
            }
        }
        for (reading_order, plans) in &planned_paragraphs {
            let paragraph = page
                .paragraphs
                .iter()
                .find(|paragraph| paragraph.reading_order == *reading_order)
                .expect("planned paragraph belongs to its page");
            for (plan, gap) in plans
                .iter()
                .filter_map(|plan| plan.section_number_gap.map(|gap| (plan, gap)))
                .filter(|(_, gap)| gap.clamped)
            {
                document
                    .diagnostics
                    .push(Diagnostic::SectionNumberGapClamped {
                        page_index: page.index,
                        reading_order: *reading_order,
                        source_title_left_pt: gap.source_title_left,
                        output_title_left_pt: gap.output_title_left,
                        gap_pt: gap.gap_pt,
                        font_size_pt: plan.font_size,
                    });
            }
            if paragraph.translated_text.as_deref() != Some(paragraph.source_text().as_str()) {
                publication_ink.push(planned_publication_ink(
                    page.index,
                    page_bounds,
                    paragraph,
                    plans,
                    &output_fonts,
                )?);
            }
            for plan in plans {
                reused_fonts.extend(
                    plan.formula_relocations
                        .iter()
                        .flat_map(|relocation| relocation.source_fonts.iter().cloned()),
                );
                install_typeset_replacements(
                    plan,
                    &output_fonts,
                    &streams,
                    &content_transforms,
                    &text_show_states,
                    &mut replacements,
                )?;
                typeset_characters.extend(planned_characters(plan, &output_fonts));
            }
        }
        let embedded_fonts = output_fonts
            .into_values()
            .map(|output| output.font)
            .collect();
        let typeset_ink_bounds = planned_paragraphs
            .iter()
            .flat_map(|(_, plans)| plans)
            .flat_map(|plan| plan.ink_bounds.iter().copied())
            .collect();
        if replacements.is_empty() {
            continue;
        }
        let replacements = replacements
            .into_iter()
            .map(
                |((content_object, byte_start, byte_end), replacement)| ContentSpanReplacement {
                    content_object,
                    byte_start,
                    byte_end,
                    replacement,
                },
            )
            .collect();
        rewrites.push(PageRewrite {
            page_index: page.index,
            replacements,
            reused_fonts: reused_fonts.into_iter().collect(),
            embedded_fonts,
            typeset_characters,
            typeset_ink_bounds,
        });
    }
    for (page_index, reading_order, reason) in preserved {
        if let Some(paragraph) = document.il.pages[page_index]
            .paragraphs
            .iter_mut()
            .find(|paragraph| paragraph.reading_order == reading_order)
        {
            paragraph.translated_text = None;
            paragraph.preserved = Some(reason);
        }
    }
    document.il.publication_ink = publication_ink;
    document.rewrites = rewrites;
    Ok(())
}

fn paragraph_plans_leave_orphan_source_ink(
    page: &il::Page,
    paragraph: &Paragraph,
    extracted: &ExtractedPage,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    plans: &[TypesetPlan],
) -> Result<bool> {
    if plans_move_formula_ink_owned_by_another_paragraph(
        page,
        paragraph,
        extracted,
        content_objects,
        plans,
    ) {
        return Ok(true);
    }
    let claimed = plans
        .iter()
        .flat_map(plan_modified_spans)
        .collect::<BTreeSet<_>>();
    let moved_formula_spans = plans
        .iter()
        .flat_map(|plan| &plan.formula_relocations)
        .filter(|relocation| {
            relocation.delta_x_pt.abs() > 0.01 || relocation.delta_y_pt.abs() > 0.01
        })
        .flat_map(|relocation| relocation.spans.iter().copied())
        .collect::<BTreeSet<_>>();
    let owner_chars = paragraph
        .chars()
        .iter()
        .filter(|character| {
            character.layout.is_some_and(|layout| {
                layout.label == LayoutLabel::InlineFormula
                    && layout.policy == TranslationPolicy::Passthrough
            })
        })
        .filter_map(|character| {
            let object = unique_page_content(character, content_objects)
                .ok()
                .flatten()?;
            moved_formula_spans
                .contains(&span_key(character, object))
                .then_some(character)
        })
        .collect::<Vec<_>>();
    let Some(owner_bounds) = owner_chars
        .iter()
        .map(|character| character.r#box.union(character.visual_bbox))
        .reduce(Rect::union)
    else {
        return Ok(false);
    };
    let em = owner_chars
        .iter()
        .map(|character| character.font_size)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(0.0_f64, f64::max);
    if em <= 0.0 {
        return Ok(false);
    }
    for path in &extracted.vector_paths {
        let span = (path.content_object, path.byte_start, path.byte_end);
        if claimed.contains(&span) {
            continue;
        }
        let Some(bounds) = walked_vector_path_bounds(path) else {
            continue;
        };
        let left = bounds.left;
        let right = bounds.right;
        let y = (bounds.bottom + bounds.top) / 2.0;
        let width = right - left;
        let overlap = (right.min(owner_bounds.right) - left.max(owner_bounds.left)).max(0.0);
        if width > 0.01
            && width <= owner_bounds.right - owner_bounds.left + em
            && overlap >= width.min(owner_bounds.right - owner_bounds.left) * 0.5
            && y >= owner_bounds.bottom - em * 0.25
            && y <= owner_bounds.top + em * 0.5
        {
            return Ok(true);
        }
    }
    for image in &extracted.inline_images {
        let span = (image.content_object, image.byte_start, image.byte_end);
        if claimed.contains(&span) {
            continue;
        }
        let Some(bounds) = walked_image_bounds(image) else {
            continue;
        };
        let width = bounds.right - bounds.left;
        let height = bounds.top - bounds.bottom;
        if width <= owner_bounds.right - owner_bounds.left + em
            && height <= owner_bounds.top - owner_bounds.bottom + em
            && bounds.right >= owner_bounds.left - em * 0.25
            && bounds.left <= owner_bounds.right + em * 0.25
            && bounds.top >= owner_bounds.bottom - em * 0.25
            && bounds.bottom <= owner_bounds.top + em * 0.25
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn plans_move_formula_ink_owned_by_another_paragraph(
    page: &il::Page,
    paragraph: &Paragraph,
    extracted: &ExtractedPage,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    plans: &[TypesetPlan],
) -> bool {
    let content_object_numbers = content_objects
        .iter()
        .map(|object_id| object_id.0)
        .collect::<BTreeSet<_>>();
    let other_units = page
        .paragraphs
        .iter()
        .filter(|candidate| candidate.reading_order != paragraph.reading_order)
        .filter_map(|candidate| {
            source_formula_units(
                candidate,
                extracted,
                content_objects,
                &content_object_numbers,
                false,
            )
        })
        .flatten()
        .collect::<Vec<_>>();
    if other_units.is_empty() {
        return false;
    }
    plans
        .iter()
        .flat_map(|plan| &plan.formula_relocations)
        .filter(|relocation| {
            relocation.delta_x_pt.abs() > 0.01 || relocation.delta_y_pt.abs() > 0.01
        })
        .any(|relocation| {
            relocation.vector_paths.iter().any(|replay| {
                extracted
                    .vector_paths
                    .iter()
                    .find(|path| {
                        (path.content_object, path.byte_start, path.byte_end) == replay.span
                    })
                    .is_none_or(|path| {
                        other_units
                            .iter()
                            .any(|unit| formula_owns_vector_path(unit, path))
                    })
            }) || relocation.inline_images.iter().any(|replay| {
                extracted
                    .inline_images
                    .iter()
                    .find(|image| {
                        (image.content_object, image.byte_start, image.byte_end) == replay.span
                    })
                    .is_none_or(|image| {
                        other_units
                            .iter()
                            .any(|unit| formula_owns_inline_image(unit, image))
                    })
            })
        })
}

/// 逐段 `typeset_overflow` 明细。之前 NDJSON 里只有 `degradation_summary` 一行汇总，
/// 无法回答「哪个障碍挡住了哪个字号」；这里公开容器盒、按与容器重叠面积排序的有界
/// 障碍样本，以及 `plan_text_segment` 实际走过的字号序列（`preferred` 起每次 −0.5，
/// 下探到 `MIN_FONT_SIZE_PT`）。
fn typeset_overflow_detail(
    page_index: usize,
    paragraph: &Paragraph,
    obstacles: &[Rect],
) -> Diagnostic {
    let chars = paragraph.chars();
    let container = chars
        .iter()
        .filter_map(|character| character.layout.map(|layout| layout.bounds))
        .reduce(Rect::union)
        .unwrap_or(Rect {
            left: 0.0,
            bottom: 0.0,
            right: 0.0,
            top: 0.0,
        });
    let mut attempted_font_sizes_pt = Vec::new();
    if !chars.is_empty() {
        let char_refs = chars.iter().collect::<Vec<_>>();
        let preferred = preferred_typeset_font_size(&char_refs).unwrap_or(chars[0].font_size);
        let mut size = preferred.max(MIN_FONT_SIZE_PT);
        loop {
            attempted_font_sizes_pt.push(size);
            if size <= MIN_FONT_SIZE_PT + 0.001 {
                break;
            }
            size = (size - 0.5).max(MIN_FONT_SIZE_PT);
        }
        attempted_font_sizes_pt.truncate(MAX_REPORTED_TYPESET_FONT_SIZES);
    }
    let mut overlapping = obstacles
        .iter()
        .map(|obstacle| (intersection_area(container, *obstacle), *obstacle))
        .filter(|(area, _)| *area > 0.0)
        .collect::<Vec<_>>();
    overlapping.sort_by(|left, right| right.0.total_cmp(&left.0));
    Diagnostic::TypesetOverflowDetail {
        page_index,
        paragraph_index: paragraph.reading_order,
        container: [
            container.left,
            container.bottom,
            container.right,
            container.top,
        ],
        attempted_font_sizes_pt,
        obstacle_count: overlapping.len(),
        obstacles: overlapping
            .iter()
            .take(MAX_REPORTED_TYPESET_OBSTACLES)
            .map(|(_, obstacle)| [obstacle.left, obstacle.bottom, obstacle.right, obstacle.top])
            .collect(),
    }
}

fn paragraph_typeset_obstacles(
    page: &il::Page,
    extracted: &ExtractedPage,
    paragraph: &Paragraph,
    include_owner_formula: bool,
    excluded_vector_spans: &BTreeSet<SpanKey>,
) -> Vec<Rect> {
    let mut obstacles = page
        .paragraphs
        .iter()
        .flat_map(|candidate| {
            candidate.chars().iter().filter(move |character| {
                candidate.reading_order != paragraph.reading_order
                    || (include_owner_formula
                        && character.layout.is_some_and(|layout| {
                            layout.label == LayoutLabel::InlineFormula
                                && layout.policy == TranslationPolicy::Passthrough
                        }))
            })
        })
        .filter(|character| character.visible && rect_is_finite(character.visual_bbox))
        .map(|character| character.visual_bbox)
        .collect::<Vec<_>>();
    obstacles.extend(
        extracted
            .layout_regions
            .iter()
            .filter(|region| {
                !paragraph.chars().iter().any(|character| {
                    point_in_rect(character.baseline_origin, region.bounds)
                        || character.layout.is_some_and(|assignment| {
                            assignment.bounds == region.bounds
                                && assignment.reading_order == region.reading_order
                                && assignment.source == region.source
                        })
                })
            })
            .map(|region| region.bounds)
            .filter(|bounds| rect_is_finite(*bounds)),
    );
    let has_inline_formula = paragraph.chars().iter().any(|character| {
        character
            .layout
            .is_some_and(|layout| layout.label == LayoutLabel::InlineFormula)
    });
    if !has_inline_formula {
        obstacles.extend(extracted.vector_paths.iter().filter_map(|path| {
            let span = (path.content_object, path.byte_start, path.byte_end);
            if excluded_vector_spans.contains(&span) {
                return None;
            }
            walked_vector_path_bounds(path)
        }));
        obstacles.extend(
            extracted
                .inline_images
                .iter()
                .filter_map(walked_image_bounds)
                .filter(|bounds| {
                    rect_is_finite(*bounds)
                        && bounds.right > bounds.left + 0.01
                        && bounds.top > bounds.bottom + 0.01
                }),
        );
    }
    obstacles
}

fn uniquely_owned_text_underlines(
    paragraph: &Paragraph,
    extracted: &ExtractedPage,
    content_objects: &BTreeSet<lopdf::ObjectId>,
) -> Option<Vec<OwnedTextUnderline>> {
    if paragraph.chars().iter().any(|character| {
        character.layout.is_some_and(|layout| {
            layout.label == LayoutLabel::InlineFormula
                && layout.policy == TranslationPolicy::Passthrough
        })
    }) {
        return Some(Vec::new());
    }
    let mut owners = BTreeMap::<SpanKey, Vec<&Char>>::new();
    for character in paragraph.chars().iter().filter(|character| {
        character.visible
            && character.text_transform == TextTransform::Upright
            && character
                .layout
                .is_some_and(|layout| layout.policy == TranslationPolicy::Translate)
    }) {
        let object_id = unique_page_content(character, content_objects)
            .ok()
            .flatten()?;
        owners
            .entry(span_key(character, object_id))
            .or_default()
            .push(character);
    }

    let mut underlines = Vec::new();
    for path in &extracted.vector_paths {
        let possible = owners
            .iter()
            .filter(|(_, characters)| text_show_may_own_underline(characters, path))
            .collect::<Vec<_>>();
        if possible.is_empty() {
            continue;
        }
        let proven = possible
            .iter()
            .filter(|(span, characters)| {
                text_show_owns_underline(
                    paragraph,
                    extracted,
                    content_objects,
                    **span,
                    characters,
                    path,
                )
            })
            .collect::<Vec<_>>();
        let [(owner_span, owner_characters)] = proven.as_slice() else {
            return None;
        };
        if possible.len() != 1 {
            return None;
        }
        let owner_origin = owner_characters
            .iter()
            .map(|character| character.baseline_origin)
            .min_by(|left, right| left.x.total_cmp(&right.x))?;
        let bounds = horizontal_path_bounds(path)?;
        underlines.push(OwnedTextUnderline {
            replay: FormulaVectorReplay {
                span: (path.content_object, path.byte_start, path.byte_end),
                content_transform: path.content_transform,
                bounds,
            },
            owner_span: **owner_span,
            owner_origin,
            bounds,
        });
    }
    Some(underlines)
}

fn horizontal_path_bounds(path: &crate::walk::WalkedVectorPath) -> Option<Rect> {
    walked_vector_path_bounds(path)
}

fn walked_vector_path_bounds(path: &crate::walk::WalkedVectorPath) -> Option<Rect> {
    let left = path.start.x.min(path.end.x);
    let right = path.start.x.max(path.end.x);
    let y = (path.start.y + path.end.y) / 2.0;
    let bounds = Rect {
        left,
        bottom: y - 0.01,
        right,
        top: y + 0.01,
    };
    if right <= left + 0.01
        || !rect_is_finite(bounds)
        || path
            .clips
            .iter()
            .any(|clip| !walked_clip_intersects_rect(clip, bounds))
    {
        return None;
    }
    clipped_ink_bounds(bounds, path.form_clip, &path.clips)
}

fn walked_image_bounds(image: &crate::walk::WalkedInlineImage) -> Option<Rect> {
    if image
        .clips
        .iter()
        .any(|clip| !walked_clip_intersects_rect(clip, image.bounds))
    {
        return None;
    }
    clipped_ink_bounds(image.bounds, image.form_clip, &image.clips)
}

fn clipped_ink_bounds(
    bounds: Rect,
    form_clip: Option<Rect>,
    clips: &crate::walk::WalkedClipStack,
) -> Option<Rect> {
    let mut bounds = bounds;
    for clip in form_clip
        .into_iter()
        .chain(clips.iter().filter_map(|clip| clip.bounds))
    {
        bounds = Rect {
            left: bounds.left.max(clip.left),
            bottom: bounds.bottom.max(clip.bottom),
            right: bounds.right.min(clip.right),
            top: bounds.top.min(clip.top),
        };
        if bounds.right <= bounds.left || bounds.top <= bounds.bottom {
            return None;
        }
    }
    Some(bounds)
}

fn text_show_may_own_underline(characters: &[&Char], path: &crate::walk::WalkedVectorPath) -> bool {
    let Some(owner_bounds) = characters
        .iter()
        .map(|character| character.r#box)
        .reduce(Rect::union)
    else {
        return false;
    };
    let em = characters
        .iter()
        .map(|character| character.font_size)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(0.0_f64, f64::max);
    let baseline = characters
        .iter()
        .map(|character| character.baseline_origin.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let Some(path_bounds) = horizontal_path_bounds(path) else {
        return false;
    };
    let owner_width = owner_bounds.right - owner_bounds.left;
    let path_width = path_bounds.right - path_bounds.left;
    let overlap = (owner_bounds.right.min(path_bounds.right)
        - owner_bounds.left.max(path_bounds.left))
    .max(0.0);
    em > 0.0
        && baseline.is_finite()
        && owner_width > 0.01
        && path_width >= owner_width * 0.75
        && path_width <= owner_width * 1.25
        && overlap >= owner_width.min(path_width) * 0.8
        && path_bounds.top <= baseline - em * 0.02
        && path_bounds.bottom >= baseline - em * 0.35
}

fn text_show_owns_underline(
    paragraph: &Paragraph,
    extracted: &ExtractedPage,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    span: SpanKey,
    characters: &[&Char],
    path: &crate::walk::WalkedVectorPath,
) -> bool {
    if !path.safe_to_replay
        || span.0 != path.content_object
        || !paragraph_owns_walked_span(paragraph, extracted, span, content_objects)
    {
        return false;
    }
    let Some(owner_bounds) = characters
        .iter()
        .map(|character| character.r#box)
        .reduce(Rect::union)
    else {
        return false;
    };
    let Some(path_bounds) = horizontal_path_bounds(path) else {
        return false;
    };
    let em = characters
        .iter()
        .map(|character| character.font_size)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(0.0_f64, f64::max);
    let baseline = characters[0].baseline_origin.y;
    if em <= 0.0
        || characters
            .iter()
            .any(|character| (character.baseline_origin.y - baseline).abs() > em * 0.05)
    {
        return false;
    }
    let owner_width = owner_bounds.right - owner_bounds.left;
    let path_width = path_bounds.right - path_bounds.left;
    let overlap = (owner_bounds.right.min(path_bounds.right)
        - owner_bounds.left.max(path_bounds.left))
    .max(0.0);
    let byte_distance = if path.byte_end <= span.1 {
        span.1 - path.byte_end
    } else {
        path.byte_start.saturating_sub(span.2)
    };
    path_width >= owner_width * 0.75
        && path_width <= owner_width * 1.25
        && overlap >= owner_width.min(path_width) * 0.8
        && path_bounds.top <= baseline - em * 0.02
        && path_bounds.bottom >= baseline - em * 0.35
        && byte_distance <= 256
}

fn attach_text_underlines_to_plans(
    plans: &mut [TypesetPlan],
    underlines: &[OwnedTextUnderline],
    page_bounds: Rect,
    typeset_container: Rect,
    obstacles: &[Rect],
) -> Option<()> {
    let mut relocated = Vec::new();
    for underline in underlines {
        let matching = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| plan.spans.first() == Some(&underline.owner_span))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [plan_index] = matching.as_slice() else {
            return None;
        };
        let plan = &plans[*plan_index];
        if plan.lines.len() != 1 || plan.baselines.len() != 1 {
            return None;
        }
        let delta_x_pt = plan.baselines[0].0 - underline.owner_origin.x;
        let delta_y_pt = plan.baselines[0].1 - underline.owner_origin.y;
        if !delta_x_pt.is_finite() || !delta_y_pt.is_finite() {
            return None;
        }
        let bounds = translated_rect(underline.bounds, delta_x_pt, delta_y_pt);
        relocated.push((*plan_index, underline, delta_x_pt, delta_y_pt, bounds));
    }
    let relocated_bounds = relocated
        .iter()
        .map(|(_, _, _, _, bounds)| *bounds)
        .collect::<Vec<_>>();
    if relocated_bounds
        .iter()
        .any(|&bounds| !rect_contains(typeset_container, bounds, 0.01))
        || !ink_bounds_are_safe(&relocated_bounds, page_bounds, obstacles)
    {
        return None;
    }
    for (plan_index, underline, delta_x_pt, delta_y_pt, bounds) in relocated {
        plans[plan_index]
            .text_vector_relocations
            .push(TextVectorRelocation {
                replay: underline.replay,
                delta_x_pt,
                delta_y_pt,
            });
        plans[plan_index].ink_bounds.push(bounds);
    }
    Some(())
}

fn plan_modified_spans(plan: &TypesetPlan) -> impl Iterator<Item = SpanKey> + '_ {
    plan.spans
        .iter()
        .copied()
        .chain(
            plan.text_vector_relocations
                .iter()
                .map(|relocation| relocation.replay.span),
        )
        .chain(plan.formula_relocations.iter().flat_map(|relocation| {
            relocation
                .spans
                .iter()
                .copied()
                .chain(relocation.vector_paths.iter().map(|path| path.span))
                .chain(relocation.inline_images.iter().map(|image| image.span))
        }))
}

pub fn font_embed(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    if document
        .rewrites
        .iter()
        .any(|value| value.reused_fonts.is_empty() && value.embedded_fonts.is_empty())
    {
        return Err(MimusError::input(
            InputReason::UnsupportedPdf,
            "FontEmbed found a rewrite with no reusable input font",
        ));
    }
    Ok(())
}

type SpanKey = (lopdf::ObjectId, usize, usize);

#[derive(Debug, Clone, Copy)]
struct TextShowState {
    line_matrix: [f64; 6],
    matrix_after_show: [f64; 6],
    font_size: f64,
    horizontal_scale: f64,
}

#[derive(Debug)]
struct TypesetPlan {
    spans: Vec<SpanKey>,
    lines: Vec<Vec<crate::translate::StyledCharacter>>,
    baselines: Vec<(f64, f64)>,
    section_number_gap: Option<PlannedSectionNumberGap>,
    formula_relocations: Vec<FormulaRelocation>,
    text_vector_relocations: Vec<TextVectorRelocation>,
    ink_bounds: Vec<Rect>,
    font_size: f64,
    single_line_expansion: Option<SingleLineBoundsExpansion>,
    multi_line_expansion: Option<MultiLineBoundsExpansion>,
}

#[derive(Debug, Clone, Copy)]
struct SourceSectionNumberGeometry {
    prefix_glyph_count: usize,
    prefix_in_output: bool,
    source_prefix_left: f64,
    source_prefix_right: f64,
    source_title_left: f64,
}

#[derive(Debug, Clone, Copy)]
struct PlannedSectionNumberGap {
    prefix_glyph_count: usize,
    prefix_in_output: bool,
    source_prefix_left: f64,
    source_title_left: f64,
    output_prefix_width: f64,
    output_title_left: f64,
    gap_pt: f64,
    clamped: bool,
}

#[derive(Debug)]
struct FormulaRelocation {
    spans: Vec<SpanKey>,
    split_glyphs: BTreeMap<SpanKey, Vec<FormulaGlyphReplay>>,
    vector_paths: Vec<FormulaVectorReplay>,
    inline_images: Vec<FormulaVectorReplay>,
    delta_x_pt: f64,
    delta_y_pt: f64,
    characters: Vec<TypesetCharacter>,
    text_ink_bounds: Rect,
    glyph_ink_bounds: Vec<Rect>,
    source_fonts: Vec<il::FontRef>,
}

#[derive(Debug, Clone, Copy)]
struct FormulaVectorReplay {
    span: SpanKey,
    content_transform: [f64; 6],
    bounds: Rect,
}

#[derive(Debug, Clone, Copy)]
struct OwnedTextUnderline {
    replay: FormulaVectorReplay,
    owner_span: SpanKey,
    owner_origin: il::Point,
    bounds: Rect,
}

#[derive(Debug, Clone, Copy)]
struct TextVectorRelocation {
    replay: FormulaVectorReplay,
    delta_x_pt: f64,
    delta_y_pt: f64,
}

#[derive(Debug, Clone)]
struct FormulaGlyphReplay {
    encoded: Vec<u8>,
    text_matrix: [f64; 6],
    validation_baseline: il::Point,
    font_resource_name: String,
    font_size: f64,
}

#[derive(Debug, Clone, Copy)]
struct TypesetLineSlot {
    left: f64,
    right: f64,
    baseline_y: f64,
}

#[derive(Debug, Clone)]
struct FormulaContinuityText {
    segment_index: usize,
    lines: Vec<FormulaContinuityLine>,
}

#[derive(Debug, Clone, Copy)]
struct FormulaContinuityLine {
    bounds: Rect,
    line_left: f64,
    starts_with_punctuation: bool,
    ends_with_punctuation: bool,
}

#[derive(Debug, Clone, Copy)]
struct FormulaContinuityFormula {
    formula_index: usize,
    bounds: Rect,
    line_left: f64,
}

#[derive(Debug, Clone, Copy)]
struct SingleLineBoundsExpansion {
    top_pt: f64,
    bottom_pt: f64,
}

#[derive(Debug, Clone, Copy)]
struct MultiLineBoundsExpansion {
    top_pt: f64,
    bottom_pt: f64,
}

#[derive(Clone, Copy, Debug)]
struct SingleLineInkFit {
    baseline_y: f64,
    ink_bounds: Rect,
    expansion: Option<SingleLineBoundsExpansion>,
}

struct BuiltOutputFont {
    font: EmbeddedFont,
    cids: BTreeMap<char, u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OutputFontKey {
    PrimaryRegular,
    PrimaryBold,
    FallbackRegular,
    FallbackBold,
}

impl OutputFontKey {
    const ALL: [Self; 4] = [
        Self::PrimaryRegular,
        Self::PrimaryBold,
        Self::FallbackRegular,
        Self::FallbackBold,
    ];

    const fn is_bold(self) -> bool {
        matches!(self, Self::PrimaryBold | Self::FallbackBold)
    }

    const fn for_style(bold: bool, fallback: bool) -> Self {
        match (bold, fallback) {
            (false, false) => Self::PrimaryRegular,
            (true, false) => Self::PrimaryBold,
            (false, true) => Self::FallbackRegular,
            (true, true) => Self::FallbackBold,
        }
    }

    const fn resource_name(self) -> &'static str {
        match self {
            Self::PrimaryRegular => "MimusR",
            Self::PrimaryBold => "MimusB",
            Self::FallbackRegular => "MimusFR",
            Self::FallbackBold => "MimusFB",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct OutputFontVariation {
    tag: [u8; 4],
    value: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct OutputFontInstance {
    variations: Vec<OutputFontVariation>,
    postscript_name: Option<String>,
}

fn output_font_instance(
    bytes: &[u8],
    key: OutputFontKey,
) -> std::result::Result<OutputFontInstance, ()> {
    use skrifa::MetadataProvider;

    let font = skrifa::FontRef::from_index(bytes, 0).map_err(|_| ())?;
    let axes = font.axes().iter().collect::<Vec<_>>();
    if axes.is_empty() {
        return Ok(OutputFontInstance {
            variations: Vec::new(),
            postscript_name: None,
        });
    }

    let slot_name = if key.is_bold() { "Bold" } else { "Regular" };
    let instances = font.named_instances();
    for index in 0..instances.len() {
        let Some(instance) = instances.get(index) else {
            continue;
        };
        let name = font
            .localized_strings(instance.subfamily_name_id())
            .english_or_first()
            .map(|value| value.to_string());
        if name.as_deref() != Some(slot_name) {
            continue;
        }
        let values = instance.user_coords().collect::<Vec<_>>();
        if values.len() != axes.len() {
            return Err(());
        }
        let postscript_name = instance
            .postscript_name_id()
            .and_then(|id| font.localized_strings(id).english_or_first())
            .map(|value| sanitize_output_font_name(&value.to_string()))
            .filter(|value| !value.is_empty());
        return Ok(OutputFontInstance {
            variations: axes
                .iter()
                .zip(values)
                .map(|(axis, value)| OutputFontVariation {
                    tag: axis.tag().to_be_bytes(),
                    value,
                })
                .collect(),
            postscript_name,
        });
    }

    let weight_tag = skrifa::Tag::new(b"wght");
    let target_weight: f32 = if key.is_bold() { 700.0 } else { 400.0 };
    Ok(OutputFontInstance {
        variations: axes
            .iter()
            .find(|axis| axis.tag() == weight_tag)
            .map(|axis| OutputFontVariation {
                tag: axis.tag().to_be_bytes(),
                value: target_weight.clamp(axis.min_value(), axis.max_value()),
            })
            .into_iter()
            .collect(),
        postscript_name: None,
    })
}

fn configured_output_font_face(
    bytes: &[u8],
    key: OutputFontKey,
) -> std::result::Result<ttf_parser::Face<'_>, ()> {
    let mut face = ttf_parser::Face::parse(bytes, 0).map_err(|_| ())?;
    for variation in output_font_instance(bytes, key)?.variations {
        face.set_variation(ttf_parser::Tag::from_bytes(&variation.tag), variation.value)
            .ok_or(())?;
    }
    Ok(face)
}

fn sanitize_output_font_name(value: &str) -> String {
    value
        .chars()
        .filter(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
        .collect()
}

fn fallback_output_font_name(bytes: &[u8], source_name: &str, key: OutputFontKey) -> String {
    use skrifa::MetadataProvider;

    let family = skrifa::FontRef::from_index(bytes, 0)
        .ok()
        .and_then(|font| {
            font.localized_strings(skrifa::string::StringId::TYPOGRAPHIC_FAMILY_NAME)
                .english_or_first()
                .or_else(|| {
                    font.localized_strings(skrifa::string::StringId::FAMILY_NAME)
                        .english_or_first()
                })
                .map(|value| sanitize_output_font_name(&value.to_string()))
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| source_name.to_owned());
    let slot_name = if key.is_bold() { "Bold" } else { "Regular" };
    format!("{family}-{slot_name}")
}

struct OutputFontFaces<'a> {
    primary_regular: ttf_parser::Face<'a>,
    primary_bold: ttf_parser::Face<'a>,
    fallback_regular: ttf_parser::Face<'a>,
    fallback_bold: ttf_parser::Face<'a>,
}

impl<'a> OutputFontFaces<'a> {
    fn parse(fonts: &'a crate::context::OutputFonts) -> std::result::Result<Self, ()> {
        Ok(Self {
            primary_regular: configured_output_font_face(
                &fonts.regular.bytes,
                OutputFontKey::PrimaryRegular,
            )?,
            primary_bold: configured_output_font_face(
                &fonts.bold.bytes,
                OutputFontKey::PrimaryBold,
            )?,
            fallback_regular: configured_output_font_face(
                &fonts.fallback_regular.bytes,
                OutputFontKey::FallbackRegular,
            )?,
            fallback_bold: configured_output_font_face(
                &fonts.fallback_bold.bytes,
                OutputFontKey::FallbackBold,
            )?,
        })
    }

    fn face(&self, key: OutputFontKey) -> &ttf_parser::Face<'a> {
        match key {
            OutputFontKey::PrimaryRegular => &self.primary_regular,
            OutputFontKey::PrimaryBold => &self.primary_bold,
            OutputFontKey::FallbackRegular => &self.fallback_regular,
            OutputFontKey::FallbackBold => &self.fallback_bold,
        }
    }

    fn key_for(&self, value: char, bold: bool) -> Option<OutputFontKey> {
        let primary = OutputFontKey::for_style(bold, false);
        if self.face(primary).glyph_index(value).is_some() {
            return Some(primary);
        }
        let fallback = OutputFontKey::for_style(bold, true);
        self.face(fallback)
            .glyph_index(value)
            .is_some()
            .then_some(fallback)
    }

    fn face_for(
        &self,
        character: crate::translate::StyledCharacter,
    ) -> Option<&ttf_parser::Face<'a>> {
        self.key_for(character.value, character.bold)
            .map(|key| self.face(key))
    }

    fn ascent_em(&self) -> f64 {
        OutputFontKey::ALL
            .into_iter()
            .map(|key| {
                let face = self.face(key);
                f64::from(face.ascender()) / f64::from(face.units_per_em())
            })
            .fold(f64::NEG_INFINITY, f64::max)
    }

    fn descent_em(&self) -> f64 {
        OutputFontKey::ALL
            .into_iter()
            .map(|key| {
                let face = self.face(key);
                f64::from(face.descender()) / f64::from(face.units_per_em())
            })
            .fold(f64::INFINITY, f64::min)
    }
}

const MIN_FONT_SIZE_PT: f64 = 8.0;
/// `typeset_overflow_detail` 的样本上限——诊断要够定位，但不能随页面内容线性膨胀。
const MAX_REPORTED_TYPESET_OBSTACLES: usize = 4;
const MAX_REPORTED_TYPESET_FONT_SIZES: usize = 16;
const LINE_ADVANCE_EM: f64 = 1.5;
const SINGLE_LINE_MAX_VERTICAL_OVERFLOW_EM: f64 = 0.25;
const SINGLE_LINE_MAX_VERTICAL_OVERFLOW_PT: f64 = 3.0;

#[derive(Debug)]
enum TypesetPlanError<'a> {
    Preserved(il::PreservedReason),
    MissingGlyphs {
        missing_characters: String,
        primary_font: &'a OutputFont,
        fallback_font: &'a OutputFont,
    },
}

fn unique_page_content(
    character: &Char,
    content_objects: &BTreeSet<lopdf::ObjectId>,
) -> Result<Option<lopdf::ObjectId>> {
    let matching = content_objects
        .iter()
        .copied()
        .filter(|object_id| object_id.0 == character.passthrough.content_object)
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [] => Ok(None),
        [object_id] => Ok(Some(*object_id)),
        _ => Err(MimusError::internal(
            InternalReason::InvariantViolation,
            format!(
                "character references ambiguous content object {}",
                character.passthrough.content_object
            ),
        )),
    }
}

fn span_key(character: &Char, content_object: lopdf::ObjectId) -> SpanKey {
    (
        content_object,
        character.passthrough.byte_start,
        character.passthrough.byte_end,
    )
}

fn span_out_of_bounds(
    content_object: lopdf::ObjectId,
    start: usize,
    end: usize,
    len: usize,
) -> MimusError {
    MimusError::internal(
        InternalReason::InvariantViolation,
        format!(
            "content object {} span {start}..{end} exceeds {len} bytes",
            content_object.0
        ),
    )
}

struct ParagraphPlanningGeometry<'a> {
    extracted: &'a ExtractedPage,
    content_objects: &'a BTreeSet<lopdf::ObjectId>,
    page_bounds: Rect,
    obstacles: &'a [Rect],
    relocation_obstacles: &'a [Rect],
}

fn typeset_container<'a>(chars: impl IntoIterator<Item = &'a Char>) -> Option<Rect> {
    chars
        .into_iter()
        .filter_map(|character| character.layout.map(|layout| layout.bounds))
        .reduce(Rect::union)
}

fn paragraph_typeset_container(paragraph: &Paragraph) -> Option<Rect> {
    typeset_container(paragraph.chars())
}

fn plan_paragraph<'a>(
    paragraph: &Paragraph,
    translated: &str,
    restored: Option<&crate::translate::RestoredTranslation>,
    output_fonts: &'a crate::context::OutputFonts,
    geometry: &ParagraphPlanningGeometry<'_>,
) -> std::result::Result<Vec<TypesetPlan>, TypesetPlanError<'a>> {
    let all_chars = paragraph.chars();
    let content_object_numbers = geometry
        .content_objects
        .iter()
        .map(|id| id.0)
        .collect::<BTreeSet<_>>();
    let mut source_segments = source_text_segments(all_chars, &content_object_numbers);
    let mut translated_segments = if let Some(restored) = restored {
        restored.segments().to_vec()
    } else {
        let bold = all_chars.iter().any(|character| {
            character.layout.is_some_and(|layout| {
                matches!(
                    layout.label,
                    LayoutLabel::DocTitle | LayoutLabel::ParagraphTitle
                )
            })
        });
        vec![
            translated
                .chars()
                .map(|value| crate::translate::StyledCharacter { value, bold })
                .collect(),
        ]
    };
    if source_segments.len() != translated_segments.len() {
        return Err(TypesetPlanError::Preserved(
            il::PreservedReason::TypesetProtocol,
        ));
    }
    let section_number_geometry = prepare_retained_section_number_prefix(
        all_chars,
        &mut source_segments,
        &mut translated_segments,
        &content_object_numbers,
    )
    .map_err(TypesetPlanError::Preserved)?;
    let mixed_with_formula = source_segments.len() > 1;
    if mixed_with_formula {
        let formulas = raw_fixed_formula_continuity(paragraph, &content_object_numbers).ok_or(
            TypesetPlanError::Preserved(il::PreservedReason::TypesetProtocol),
        )?;
        attach_translated_radicals_to_formula_operands(
            &mut source_segments,
            &formulas,
            &mut translated_segments,
        )
        .ok_or(TypesetPlanError::Preserved(
            il::PreservedReason::TypesetProtocol,
        ))?;
        normalize_formula_interleaved_punctuation_order(
            paragraph,
            &content_object_numbers,
            &mut source_segments,
            &formulas,
            &mut translated_segments,
            formula_continuity_limit(paragraph, &content_object_numbers).ok_or(
                TypesetPlanError::Preserved(il::PreservedReason::TypesetProtocol),
            )?,
        );
    }
    let shared_formula_operand = paragraph_has_shared_formula_operand(
        paragraph,
        geometry.content_objects,
        &content_object_numbers,
    );
    let fixed_plans = source_segments
        .iter()
        .zip(&translated_segments)
        .enumerate()
        .filter(|(_, (source, translated))| !source.is_empty() || !translated.is_empty())
        .map(|(segment_index, (source, translated))| {
            let line_slots = mixed_with_formula.then(|| source_text_line_slots(paragraph, source));
            plan_text_segment(
                source,
                translated,
                geometry.content_objects,
                output_fonts,
                geometry.page_bounds,
                geometry.obstacles,
                line_slots.as_deref(),
                (!mixed_with_formula)
                    .then_some(paragraph.first_line_indent)
                    .flatten(),
                (segment_index == 0)
                    .then_some(section_number_geometry)
                    .flatten(),
            )
            .map(|plan| (segment_index, plan))
        })
        .collect::<std::result::Result<Vec<_>, _>>();
    let mut fixed_slot_overflow = false;
    match fixed_plans {
        Ok(mut indexed_plans) => {
            let formula_first = section_number_geometry.is_some_and(|geometry| {
                section_number_title_is_formula_first(&translated_segments, geometry)
            });
            if formula_first
                && !indexed_plans
                    .iter()
                    .any(|(_, plan)| plan.section_number_gap.is_some())
            {
                let (_, plan) = indexed_plans
                    .first_mut()
                    .ok_or(TypesetPlanError::Preserved(
                        il::PreservedReason::TypesetProtocol,
                    ))?;
                let faces = OutputFontFaces::parse(output_fonts).map_err(|_| {
                    TypesetPlanError::Preserved(il::PreservedReason::UnsupportedFont)
                })?;
                plan.section_number_gap = Some(
                    planned_section_number_gap(
                        &translated_segments[0],
                        &faces,
                        plan.font_size,
                        section_number_geometry.expect("formula-first section geometry exists"),
                    )
                    .ok_or(TypesetPlanError::Preserved(
                        il::PreservedReason::TypesetProtocol,
                    ))?,
                );
            }
            let fixed_formula_requires_relocation = formula_first
                && indexed_plans
                    .iter()
                    .any(|(_, plan)| plan.section_number_gap.is_some_and(|gap| gap.clamped));
            let continuity_text = planned_formula_continuity_text(&indexed_plans, output_fonts);
            let plans = indexed_plans
                .into_iter()
                .map(|(_, plan)| plan)
                .collect::<Vec<_>>();
            let paragraph_ink = plans
                .iter()
                .flat_map(|plan| plan.ink_bounds.iter().copied())
                .collect::<Vec<_>>();
            if !fixed_formula_requires_relocation && !rects_intersect_each_other(&paragraph_ink) {
                if !mixed_with_formula {
                    return Ok(plans);
                }
                let continuity_formulas =
                    fixed_formula_continuity(paragraph, &content_object_numbers);
                if let (Some(continuity_text), Some(continuity_formulas), Some(limit)) = (
                    continuity_text,
                    continuity_formulas,
                    formula_continuity_limit(paragraph, &content_object_numbers),
                ) && formula_continuity_is_valid(
                    &translated_segments,
                    &continuity_text,
                    &continuity_formulas,
                    limit,
                ) {
                    if !shared_formula_operand {
                        return Ok(plans);
                    }
                    let plans = prepare_shared_formula_fixed_plans(
                        paragraph,
                        plans,
                        geometry.extracted,
                        geometry.content_objects,
                        &content_object_numbers,
                    )
                    .ok_or(TypesetPlanError::Preserved(
                        il::PreservedReason::TypesetProtocol,
                    ))?;
                    let paragraph_ink = plans
                        .iter()
                        .flat_map(|plan| plan.ink_bounds.iter().copied())
                        .collect::<Vec<_>>();
                    if !rects_intersect_each_other(&paragraph_ink) {
                        return Ok(plans);
                    }
                }
                fixed_slot_overflow = true;
            }
        }
        Err(TypesetPlanError::Preserved(il::PreservedReason::TypesetOverflow)) => {
            fixed_slot_overflow = true;
        }
        Err(error) => return Err(error),
    }
    if mixed_with_formula && restored.is_some() && paragraph_source_line_count(paragraph) >= 2 {
        return plan_relocated_formula_flow(
            paragraph,
            &source_segments,
            &translated_segments,
            section_number_geometry,
            geometry.extracted,
            geometry.content_objects,
            output_fonts,
            geometry.page_bounds,
            geometry.relocation_obstacles,
        )
        .map(|plan| vec![plan]);
    }
    Err(TypesetPlanError::Preserved(
        if shared_formula_operand && !fixed_slot_overflow {
            il::PreservedReason::TypesetProtocol
        } else {
            il::PreservedReason::TypesetOverflow
        },
    ))
}

fn prepare_shared_formula_fixed_plans(
    paragraph: &Paragraph,
    mut plans: Vec<TypesetPlan>,
    extracted: &ExtractedPage,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    content_object_numbers: &BTreeSet<u32>,
) -> Option<Vec<TypesetPlan>> {
    let formula_units = source_formula_units(
        paragraph,
        extracted,
        content_objects,
        content_object_numbers,
        false,
    )?;
    let shared_spans = formula_units
        .iter()
        .flat_map(|unit| unit.split_glyphs.keys().copied())
        .collect::<BTreeSet<_>>();
    let owners = shared_spans
        .iter()
        .map(|span| {
            let indices = plans
                .iter()
                .enumerate()
                .filter_map(|(index, plan)| plan.spans.contains(span).then_some(index))
                .collect::<Vec<_>>();
            (*span, indices)
        })
        .collect::<BTreeMap<_, _>>();
    if owners.values().all(|indices| indices.len() == 1) {
        for unit in formula_units {
            for (span, glyphs) in unit.split_glyphs {
                let owner = owners.get(&span)?.first().copied()?;
                let matching = unit
                    .chars
                    .iter()
                    .zip(&unit.validation_characters)
                    .filter(|(character, _)| {
                        unique_page_content(character, content_objects)
                            .ok()
                            .flatten()
                            .is_some_and(|object_id| span_key(character, object_id) == span)
                    })
                    .collect::<Vec<_>>();
                let characters = matching
                    .iter()
                    .map(|(_, expected)| (*expected).clone())
                    .collect::<Vec<_>>();
                let ink_bounds = matching
                    .iter()
                    .map(|(character, _)| character.visual_bbox)
                    .reduce(Rect::union)?;
                let glyph_ink_bounds = matching
                    .iter()
                    .map(|(character, _)| character.visual_bbox)
                    .collect();
                let source_fonts = matching
                    .iter()
                    .map(|(character, _)| character.font.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                plans[owner].formula_relocations.push(FormulaRelocation {
                    spans: vec![span],
                    split_glyphs: BTreeMap::from([(span, glyphs)]),
                    vector_paths: Vec::new(),
                    inline_images: Vec::new(),
                    delta_x_pt: 0.0,
                    delta_y_pt: 0.0,
                    characters,
                    text_ink_bounds: ink_bounds,
                    glyph_ink_bounds,
                    source_fonts,
                });
                plans[owner].ink_bounds.push(ink_bounds);
            }
        }
        return Some(plans);
    }

    let font_size = plans.first()?.font_size;
    if plans.iter().any(|plan| {
        (plan.font_size - font_size).abs() > 0.001
            || plan.single_line_expansion.is_some()
            || plan.multi_line_expansion.is_some()
            || !plan.formula_relocations.is_empty()
    }) {
        return None;
    }
    let mut spans = plans
        .iter()
        .flat_map(|plan| plan.spans.iter().copied())
        .collect::<Vec<_>>();
    spans.sort_unstable();
    spans.dedup();
    if formula_units.iter().any(|unit| {
        unit.split_glyphs
            .keys()
            .any(|span| spans.binary_search(span).is_err())
    }) {
        return None;
    }
    let section_number_gap = plans.iter().find_map(|plan| plan.section_number_gap);
    let mut lines = Vec::new();
    let mut baselines = Vec::new();
    let mut ink_bounds = Vec::new();
    for plan in plans {
        lines.extend(plan.lines);
        baselines.extend(plan.baselines);
        ink_bounds.extend(plan.ink_bounds);
    }
    let formula_relocations = formula_units
        .into_iter()
        .map(|unit| {
            let characters = unit.validation_characters.clone();
            ink_bounds.push(unit.ink_bounds);
            Some(FormulaRelocation {
                spans: unit.spans,
                split_glyphs: unit.split_glyphs,
                vector_paths: unit.vector_paths,
                inline_images: unit.inline_images,
                delta_x_pt: 0.0,
                delta_y_pt: 0.0,
                characters,
                text_ink_bounds: unit.glyph_ink_bounds.iter().copied().reduce(Rect::union)?,
                glyph_ink_bounds: unit.glyph_ink_bounds,
                source_fonts: unit.source_fonts,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(vec![TypesetPlan {
        spans,
        lines,
        baselines,
        section_number_gap,
        formula_relocations,
        text_vector_relocations: Vec::new(),
        ink_bounds,
        font_size,
        single_line_expansion: None,
        multi_line_expansion: None,
    }])
}

fn paragraph_has_shared_formula_operand(
    paragraph: &Paragraph,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    content_object_numbers: &BTreeSet<u32>,
) -> bool {
    let mut span_classes = BTreeMap::<SpanKey, (bool, bool)>::new();
    for character in paragraph.chars() {
        let Some(object_id) = unique_page_content(character, content_objects)
            .ok()
            .flatten()
        else {
            continue;
        };
        let classes = span_classes
            .entry(span_key(character, object_id))
            .or_default();
        if prepared_character_class(character, content_object_numbers)
            == PreparedCharacterClass::Formula
        {
            classes.0 = true;
        } else {
            classes.1 = true;
        }
    }
    span_classes
        .into_values()
        .any(|(has_formula, has_other)| has_formula && has_other)
}

fn paragraph_source_line_count(paragraph: &Paragraph) -> usize {
    let mut baselines = Vec::<f64>::new();
    for character in paragraph.chars() {
        if !baselines
            .iter()
            .any(|baseline| (*baseline - character.baseline_origin.y).abs() <= 0.01)
        {
            baselines.push(character.baseline_origin.y);
        }
    }
    baselines.len()
}

fn plan_shared_number_identity<'a>(
    paragraph: &Paragraph,
    translated: &str,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    output_fonts: &'a crate::context::OutputFonts,
    page_bounds: Rect,
    obstacles: &[Rect],
) -> std::result::Result<TypesetPlan, TypesetPlanError<'a>> {
    let chars = paragraph.chars();
    if chars.is_empty()
        || chars.iter().any(|character| {
            !character.visible
                || character.text_transform != TextTransform::Upright
                || !character.layout.is_some_and(|layout| {
                    layout.label == LayoutLabel::Number
                        && layout.policy == TranslationPolicy::Passthrough
                })
        })
    {
        return Err(TypesetPlanError::Preserved(
            il::PreservedReason::TypesetProtocol,
        ));
    }
    let chars = chars.iter().collect::<Vec<_>>();
    let translated = translated
        .chars()
        .map(|value| crate::translate::StyledCharacter { value, bold: false })
        .collect::<Vec<_>>();
    plan_text_segment(
        &chars,
        &translated,
        content_objects,
        output_fonts,
        page_bounds,
        obstacles,
        None,
        None,
        None,
    )
}

fn source_text_line_slots(paragraph: &Paragraph, chars: &[&Char]) -> Vec<TypesetLineSlot> {
    let Some(container) = chars
        .iter()
        .filter_map(|character| character.layout.map(|layout| layout.bounds))
        .reduce(Rect::union)
    else {
        return Vec::new();
    };
    let formula_ink = paragraph
        .chars()
        .iter()
        .filter(|character| {
            character.visible
                && character.layout.is_some_and(|layout| {
                    layout.label == LayoutLabel::InlineFormula
                        && layout.policy == TranslationPolicy::Passthrough
                })
                && rect_is_finite(character.visual_bbox)
        })
        .map(|character| character.visual_bbox)
        .collect::<Vec<_>>();
    let mut source_lines = Vec::<Vec<&Char>>::new();
    for character in chars {
        if let Some(line) = source_lines
            .last_mut()
            .filter(|line| (line[0].baseline_origin.y - character.baseline_origin.y).abs() <= 0.01)
        {
            line.push(character);
        } else {
            source_lines.push(vec![character]);
        }
    }
    source_lines
        .into_iter()
        .filter_map(|line| {
            let line_box = line
                .iter()
                .map(|character| character.r#box)
                .reduce(Rect::union)?;
            let source_center_x = (line_box.left + line_box.right) / 2.0;
            let mut left = line[0].baseline_origin.x.max(container.left);
            let mut right = container.right;
            let mut line_formula_ink = formula_ink
                .iter()
                .copied()
                .filter(|ink| ink.top > line_box.bottom + 0.01 && ink.bottom < line_box.top - 0.01)
                .collect::<Vec<_>>();
            line_formula_ink.sort_by(|left, right| left.left.total_cmp(&right.left));
            for ink in line_formula_ink {
                let formula_center_x = (ink.left + ink.right) / 2.0;
                if formula_center_x < source_center_x {
                    left = left.max(ink.right);
                } else {
                    right = right.min(ink.left);
                    break;
                }
            }
            (right > left + 0.01).then_some(TypesetLineSlot {
                left,
                right,
                baseline_y: line[0].baseline_origin.y,
            })
        })
        .collect()
}

fn source_text_segments<'a>(
    chars: &'a [Char],
    content_objects: &BTreeSet<u32>,
) -> Vec<Vec<&'a Char>> {
    let mut segments = vec![Vec::new()];
    let mut start = 0;
    while start < chars.len() {
        let class = prepared_character_class(&chars[start], content_objects);
        let mut end = start + 1;
        while end < chars.len() && prepared_character_class(&chars[end], content_objects) == class {
            end += 1;
        }
        match class {
            PreparedCharacterClass::Text { .. } => {
                segments.last_mut().unwrap().extend(&chars[start..end]);
            }
            PreparedCharacterClass::Formula => segments.push(Vec::new()),
            PreparedCharacterClass::Passthrough => {}
        }
        start = end;
    }
    segments
}

fn attach_translated_radicals_to_formula_operands(
    source_segments: &mut [Vec<&Char>],
    formulas: &[FormulaContinuityFormula],
    translated_segments: &mut [Vec<crate::translate::StyledCharacter>],
) -> Option<()> {
    if !source_segments
        .iter()
        .flatten()
        .any(|character| character.unicode == Some('\u{221a}'))
    {
        return Some(());
    }
    if formulas.len() + 1 != source_segments.len()
        || translated_segments.len() != source_segments.len()
    {
        return None;
    }
    let attachments = uniquely_attached_source_radicals(source_segments, formulas)?;
    let mut by_segment = BTreeMap::<usize, Vec<usize>>::new();
    for attachment in attachments {
        by_segment
            .entry(attachment.source_segment_index)
            .or_default()
            .push(attachment.source_character_index);
    }
    for (segment_index, mut source_indices) in by_segment {
        let matching = translated_segments[segment_index]
            .iter()
            .enumerate()
            .filter_map(|(index, character)| (character.value == '\u{221a}').then_some(index))
            .collect::<Vec<_>>();
        if !matching.is_empty() && matching.len() != source_indices.len() {
            return None;
        }
        for translated_index in matching.into_iter().rev() {
            translated_segments[segment_index].remove(translated_index);
        }
        source_indices.sort_unstable();
        for source_index in source_indices.into_iter().rev() {
            source_segments[segment_index].remove(source_index);
        }
    }
    Some(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceRadicalAttachment {
    source_segment_index: usize,
    source_character_index: usize,
    formula_index: usize,
}

fn uniquely_attached_source_radicals(
    source_segments: &[Vec<&Char>],
    formulas: &[FormulaContinuityFormula],
) -> Option<Vec<SourceRadicalAttachment>> {
    let mut attachments = Vec::new();
    let mut claimed_formulas = BTreeSet::new();
    for (source_segment_index, source) in source_segments.iter().enumerate() {
        for (source_character_index, character) in source.iter().enumerate() {
            if character.unicode != Some('\u{221a}') {
                continue;
            }
            let matching = formulas
                .iter()
                .enumerate()
                .filter_map(|(formula_index, formula)| {
                    source_radical_attaches_to_formula(character, formula.bounds)
                        .then_some(formula_index)
                })
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [] => {}
                [formula_index] if claimed_formulas.insert(*formula_index) => {
                    attachments.push(SourceRadicalAttachment {
                        source_segment_index,
                        source_character_index,
                        formula_index: *formula_index,
                    });
                }
                _ => return None,
            }
        }
    }
    Some(attachments)
}

fn source_radical_attaches_to_formula(character: &Char, formula_bounds: Rect) -> bool {
    let bounds = character.r#box.union(character.visual_bbox);
    let em = character.font_size.max(0.01);
    let gap = formula_bounds.left - bounds.right;
    let overlaps_vertically =
        bounds.top > formula_bounds.bottom + 0.01 && formula_bounds.top > bounds.bottom + 0.01;
    overlaps_vertically && gap >= -0.05 * em && gap <= 0.25 * em
}

fn prepare_retained_section_number_prefix<'a>(
    all_chars: &'a [Char],
    source_segments: &mut [Vec<&'a Char>],
    translated_segments: &mut [Vec<crate::translate::StyledCharacter>],
    content_objects: &BTreeSet<u32>,
) -> std::result::Result<Option<SourceSectionNumberGeometry>, il::PreservedReason> {
    let span = |character: &Char| {
        (
            character.passthrough.content_object,
            character.passthrough.byte_start,
            character.passthrough.byte_end,
        )
    };
    let mut visual_order = (0..all_chars.len()).collect::<Vec<_>>();
    visual_order.sort_by(|&left, &right| {
        all_chars[left]
            .r#box
            .left
            .total_cmp(&all_chars[right].r#box.left)
            .then(left.cmp(&right))
    });
    let prefix_indices = visual_order
        .iter()
        .copied()
        .take_while(|&index| {
            let character = &all_chars[index];
            character.visible
                && character.text_transform == TextTransform::Upright
                && character.layout.is_some_and(|layout| {
                    layout.label == LayoutLabel::Number
                        && layout.policy == TranslationPolicy::Passthrough
                })
        })
        .collect::<Vec<_>>();
    let prefix = prefix_indices
        .iter()
        .map(|&index| &all_chars[index])
        .collect::<Vec<_>>();
    let prefix_index_set = prefix_indices.iter().copied().collect::<BTreeSet<_>>();
    if prefix.is_empty() {
        return Ok(None);
    }
    if !section_number_prefix_is_supported(&prefix) {
        return Err(il::PreservedReason::TypesetProtocol);
    }

    let translated_spans = source_segments
        .iter()
        .flatten()
        .map(|character| span(character))
        .collect::<BTreeSet<_>>();
    let shared_passthrough_indices = all_chars
        .iter()
        .enumerate()
        .filter(|(_, character)| {
            prepared_character_class(character, content_objects)
                == PreparedCharacterClass::Passthrough
                && translated_spans.contains(&span(character))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let prefix_in_output = !shared_passthrough_indices.is_empty();
    if prefix_in_output
        && (shared_passthrough_indices
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            != prefix_index_set
            || prefix
                .iter()
                .any(|character| !translated_spans.contains(&span(character))))
    {
        return Err(il::PreservedReason::TypesetProtocol);
    }

    let prefix_spans = prefix
        .iter()
        .map(|character| span(character))
        .collect::<BTreeSet<_>>();
    if prefix_in_output {
        let matching_segments = source_segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| {
                segment
                    .iter()
                    .any(|character| prefix_spans.contains(&span(character)))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matching_segments != [0]
            || all_chars
                .iter()
                .enumerate()
                .filter(|(index, _)| !prefix_index_set.contains(index))
                .any(|(_, character)| {
                    prefix_spans.contains(&span(character))
                        && !source_segments[0]
                            .iter()
                            .any(|source| std::ptr::eq(*source, character))
                })
        {
            return Err(il::PreservedReason::TypesetProtocol);
        }
    }

    let bold = translated_segments[0]
        .first()
        .map(|character| character.bold)
        .unwrap_or_else(|| {
            source_segments[0].first().is_some_and(|character| {
                matches!(
                    prepared_character_class(character, content_objects),
                    PreparedCharacterClass::Text { bold: true }
                )
            })
        });
    let source_prefix_left = prefix
        .iter()
        .map(|character| character.r#box.left)
        .min_by(f64::total_cmp)
        .ok_or(il::PreservedReason::TypesetProtocol)?;
    let source_prefix_right = prefix
        .iter()
        .map(|character| character.r#box.right)
        .max_by(f64::total_cmp)
        .ok_or(il::PreservedReason::TypesetProtocol)?;
    let prefix_line_bounds = prefix
        .iter()
        .map(|character| character.r#box.union(character.visual_bbox))
        .reduce(Rect::union)
        .ok_or(il::PreservedReason::TypesetProtocol)?;
    let title = all_chars
        .iter()
        .enumerate()
        .filter(|(index, _)| !prefix_index_set.contains(index))
        .filter(|(_, character)| {
            let bounds = character.r#box.union(character.visual_bbox);
            character.visible
                && character.text_transform == TextTransform::Upright
                && character
                    .unicode
                    .is_some_and(|value| !value.is_whitespace())
                && character.r#box.left >= source_prefix_right - 0.01
                && mimus_quality_contract::formula_items_share_line(
                    prefix_line_bounds.bottom,
                    prefix_line_bounds.top,
                    bounds.bottom,
                    bounds.top,
                )
        })
        .min_by(|(left_index, left), (right_index, right)| {
            left.r#box
                .left
                .total_cmp(&right.r#box.left)
                .then(left_index.cmp(right_index))
        })
        .map(|(_, character)| character)
        .ok_or(il::PreservedReason::TypesetProtocol)?;
    let source_title_left = title.r#box.left;
    if !source_prefix_left.is_finite()
        || !source_prefix_right.is_finite()
        || !source_title_left.is_finite()
        || source_prefix_right <= source_prefix_left
    {
        return Err(il::PreservedReason::TypesetProtocol);
    }
    if prefix_in_output {
        let mut retained = prefix
            .iter()
            .map(|character| crate::translate::StyledCharacter {
                value: character
                    .unicode
                    .expect("validated section number character"),
                bold,
            })
            .collect::<Vec<_>>();
        retained.append(&mut translated_segments[0]);
        translated_segments[0] = retained;
        let mut source = prefix.clone();
        source.append(&mut source_segments[0]);
        source_segments[0] = source;
    }
    Ok(Some(SourceSectionNumberGeometry {
        prefix_glyph_count: prefix.len(),
        prefix_in_output,
        source_prefix_left,
        source_prefix_right,
        source_title_left,
    }))
}

fn section_number_prefix_is_supported(chars: &[&Char]) -> bool {
    let values = chars
        .iter()
        .filter_map(|character| character.unicode)
        .collect::<Vec<_>>();
    if values.len() != chars.len() {
        return false;
    }
    let Some(marker_kind) = values
        .iter()
        .copied()
        .find(|character| !character.is_whitespace())
        .map(|character| character.is_ascii_digit())
    else {
        return false;
    };
    values.iter().all(|character| {
        (if marker_kind {
            character.is_ascii_digit()
        } else {
            matches!(character, 'I' | 'V' | 'X')
        }) || *character == '.'
            || character.is_whitespace()
    }) && values
        .iter()
        .any(|character| character.is_ascii_digit() || matches!(character, 'I' | 'V' | 'X'))
}

#[derive(Debug)]
struct SourceFormulaUnit<'a> {
    chars: Vec<&'a Char>,
    validation_characters: Vec<TypesetCharacter>,
    spans: Vec<SpanKey>,
    split_glyphs: BTreeMap<SpanKey, Vec<FormulaGlyphReplay>>,
    vector_paths: Vec<FormulaVectorReplay>,
    inline_images: Vec<FormulaVectorReplay>,
    bounds: Rect,
    glyph_ink_bounds: Vec<Rect>,
    ink_bounds: Rect,
    source_fonts: Vec<il::FontRef>,
}

enum FormulaFlowAtom {
    Text {
        segment_index: usize,
        characters: Vec<crate::translate::StyledCharacter>,
        extra_advance_pt: f64,
        section_prefix_only: bool,
    },
    Formula(usize),
}

struct FormulaFlowPlacement {
    lines: Vec<Vec<crate::translate::StyledCharacter>>,
    baselines: Vec<(f64, f64)>,
    line_segment_indices: Vec<usize>,
    formula_relocations: Vec<FormulaRelocation>,
    formula_line_lefts: Vec<f64>,
    ink_bounds: Vec<Rect>,
}

enum FormulaFlowAttempt {
    Placed(FormulaFlowPlacement),
    NoFit,
}

#[allow(clippy::too_many_arguments)]
fn plan_relocated_formula_flow<'a>(
    paragraph: &Paragraph,
    source_segments: &[Vec<&Char>],
    translated_segments: &[Vec<crate::translate::StyledCharacter>],
    section_number_geometry: Option<SourceSectionNumberGeometry>,
    extracted: &ExtractedPage,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    output_fonts: &'a crate::context::OutputFonts,
    page_bounds: Rect,
    obstacles: &[Rect],
) -> std::result::Result<TypesetPlan, TypesetPlanError<'a>> {
    let content_object_numbers = content_objects
        .iter()
        .map(|object_id| object_id.0)
        .collect::<BTreeSet<_>>();
    let formula_units = source_formula_units(
        paragraph,
        extracted,
        content_objects,
        &content_object_numbers,
        true,
    )
    .ok_or(TypesetPlanError::Preserved(
        il::PreservedReason::TypesetProtocol,
    ))?;
    if formula_units.len() + 1 != source_segments.len()
        || formula_units.len() + 1 != translated_segments.len()
    {
        return Err(TypesetPlanError::Preserved(
            il::PreservedReason::TypesetProtocol,
        ));
    }
    let text_chars = source_segments
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    if text_chars.is_empty() {
        return Err(TypesetPlanError::Preserved(
            il::PreservedReason::TypesetProtocol,
        ));
    }
    let mut spans = text_chars
        .iter()
        .filter_map(|character| {
            unique_page_content(character, content_objects)
                .ok()
                .flatten()
                .map(|object_id| span_key(character, object_id))
        })
        .collect::<Vec<_>>();
    spans.sort_unstable();
    spans.dedup();
    if spans.is_empty() {
        return Err(TypesetPlanError::Preserved(
            il::PreservedReason::Unlocatable,
        ));
    }
    let container = typeset_container(text_chars.iter().copied()).ok_or(
        TypesetPlanError::Preserved(il::PreservedReason::TypesetOverflow),
    )?;
    let translated_segments = translated_segments
        .iter()
        .map(|segment| normalize_typeset_whitespace(segment))
        .collect::<Vec<_>>();
    let faces = OutputFontFaces::parse(output_fonts)
        .map_err(|_| TypesetPlanError::Preserved(il::PreservedReason::UnsupportedFont))?;
    let translated = translated_segments
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    for is_bold in [false, true] {
        let missing_characters = translated
            .iter()
            .filter(|character| character.bold == is_bold)
            .map(|character| character.value)
            .filter(|value| faces.key_for(*value, is_bold).is_none())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(16)
            .collect::<String>();
        if !missing_characters.is_empty() {
            return Err(TypesetPlanError::MissingGlyphs {
                missing_characters,
                primary_font: if is_bold {
                    &output_fonts.bold
                } else {
                    &output_fonts.regular
                },
                fallback_font: if is_bold {
                    &output_fonts.fallback_bold
                } else {
                    &output_fonts.fallback_regular
                },
            });
        }
    }
    let preferred = preferred_typeset_font_size(&text_chars).unwrap_or(text_chars[0].font_size);
    let first = text_chars[0];
    let mut size = preferred.max(MIN_FONT_SIZE_PT);
    loop {
        let section_number_gap = section_number_geometry
            .map(|source| {
                planned_section_number_gap(&translated_segments[0], &faces, size, source).ok_or(
                    TypesetPlanError::Preserved(il::PreservedReason::TypesetProtocol),
                )
            })
            .transpose()?;
        let inline_section_number_gap = section_number_gap.filter(|gap| gap.prefix_in_output);
        let atoms = formula_flow_atoms(&translated_segments, inline_section_number_gap);
        if formula_flow_kinsoku_is_unsatisfiable(&atoms) {
            return Err(TypesetPlanError::Preserved(
                il::PreservedReason::TypesetOverflow,
            ));
        }
        let ascent = faces.ascent_em() * size;
        let descent = faces.descent_em() * size;
        let first_y = first.baseline_origin.y.min(container.top - ascent);
        let may_expand = size <= MIN_FONT_SIZE_PT + 0.001;
        let first_line_start_x = section_number_gap.map_or_else(
            || first.baseline_origin.x.max(container.left),
            |gap| {
                if gap.prefix_in_output {
                    gap.source_prefix_left
                } else {
                    gap.output_title_left
                }
            },
        );
        let first_line_indent =
            section_number_gap.map(|_| (first_line_start_x - container.left).max(0.0));
        let mut slots = obstacle_aware_multiline_slots(
            container,
            first_y,
            ascent,
            descent,
            size,
            page_bounds,
            obstacles,
            first_line_indent,
        );
        if !may_expand {
            slots.retain(|slot| slot.baseline_y + descent >= container.bottom - 0.01);
        }
        let placement = match place_formula_flow(
            &atoms,
            &formula_units,
            &faces,
            size,
            &slots,
            inline_section_number_gap,
            section_number_gap.is_some(),
        )
        .ok_or(TypesetPlanError::Preserved(
            il::PreservedReason::TypesetProtocol,
        ))? {
            FormulaFlowAttempt::Placed(placement) => placement,
            FormulaFlowAttempt::NoFit => {
                // This is still geometric packing before the continuity oracle.
                if size <= MIN_FONT_SIZE_PT + 0.001 {
                    break;
                }
                size = (size - 0.5).max(MIN_FONT_SIZE_PT);
                continue;
            }
        };
        if ink_bounds_are_safe(&placement.ink_bounds, page_bounds, obstacles) {
            let continuity_text =
                formula_flow_continuity_text(&placement, &faces, size, inline_section_number_gap)
                    .ok_or(TypesetPlanError::Preserved(
                    il::PreservedReason::TypesetProtocol,
                ))?;
            let continuity_formulas = formula_units
                .iter()
                .zip(&placement.formula_relocations)
                .zip(&placement.formula_line_lefts)
                .enumerate()
                .map(
                    |(formula_index, ((unit, relocation), line_left))| FormulaContinuityFormula {
                        formula_index,
                        bounds: translated_rect(
                            unit.bounds,
                            relocation.delta_x_pt,
                            relocation.delta_y_pt,
                        ),
                        line_left: *line_left,
                    },
                )
                .collect::<Vec<_>>();
            let continuity_limit = formula_continuity_limit(paragraph, &content_object_numbers)
                .ok_or(TypesetPlanError::Preserved(
                    il::PreservedReason::TypesetProtocol,
                ))?;
            if !formula_continuity_is_valid(
                &translated_segments,
                &continuity_text,
                &continuity_formulas,
                continuity_limit,
            ) {
                return Err(TypesetPlanError::Preserved(
                    il::PreservedReason::TypesetOverflow,
                ));
            }
            let overflow_top = placement
                .ink_bounds
                .iter()
                .map(|ink| ink.top - container.top)
                .fold(0.0, f64::max)
                .max(0.0);
            let overflow_bottom = placement
                .ink_bounds
                .iter()
                .map(|ink| container.bottom - ink.bottom)
                .fold(0.0, f64::max)
                .max(0.0);
            let fits_container = overflow_top <= 0.01 && overflow_bottom <= 0.01;
            if fits_container || may_expand {
                return Ok(TypesetPlan {
                    spans,
                    lines: placement.lines,
                    baselines: placement.baselines,
                    section_number_gap,
                    formula_relocations: placement.formula_relocations,
                    text_vector_relocations: Vec::new(),
                    ink_bounds: placement.ink_bounds,
                    font_size: size,
                    single_line_expansion: None,
                    multi_line_expansion: (!fits_container).then_some(MultiLineBoundsExpansion {
                        top_pt: overflow_top,
                        bottom_pt: overflow_bottom,
                    }),
                });
            }
        }
        if size <= MIN_FONT_SIZE_PT + 0.001 {
            break;
        }
        size = (size - 0.5).max(MIN_FONT_SIZE_PT);
    }
    Err(TypesetPlanError::Preserved(
        il::PreservedReason::TypesetOverflow,
    ))
}

fn source_formula_units<'a>(
    paragraph: &'a Paragraph,
    extracted: &ExtractedPage,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    content_object_numbers: &BTreeSet<u32>,
    attach_relocated_ink: bool,
) -> Option<Vec<SourceFormulaUnit<'a>>> {
    let mut span_classes = BTreeMap::<SpanKey, (bool, bool, bool)>::new();
    for character in paragraph.chars() {
        let object_id = unique_page_content(character, content_objects)
            .ok()
            .flatten()?;
        let entry = span_classes
            .entry(span_key(character, object_id))
            .or_default();
        match prepared_character_class(character, content_object_numbers) {
            PreparedCharacterClass::Formula => entry.0 = true,
            PreparedCharacterClass::Text { .. } => entry.1 = true,
            PreparedCharacterClass::Passthrough => entry.2 = true,
        }
    }
    let mut grouped = Vec::<Vec<&Char>>::new();
    let mut current = Vec::new();
    for character in paragraph.chars() {
        if prepared_character_class(character, content_object_numbers)
            == PreparedCharacterClass::Formula
        {
            current.push(character);
        } else if !current.is_empty() {
            grouped.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        grouped.push(current);
    }
    let mut units = grouped
        .into_iter()
        .map(|chars| {
            if chars.iter().any(|character| {
                character.unicode.is_none()
                    || !character.visible
                    || character.text_transform != TextTransform::Upright
                    || !rect_is_finite(character.r#box)
                    || !rect_is_finite(character.visual_bbox)
                    || !character.baseline_origin.x.is_finite()
                    || !character.baseline_origin.y.is_finite()
            }) {
                return None;
            }
            let mut spans = chars
                .iter()
                .filter_map(|character| {
                    unique_page_content(character, content_objects)
                        .ok()
                        .flatten()
                        .map(|object_id| span_key(character, object_id))
                })
                .collect::<Vec<_>>();
            spans.sort_unstable();
            spans.dedup();
            if spans.is_empty() {
                return None;
            }
            let mut split_glyphs = BTreeMap::new();
            for span in &spans {
                let &(has_formula, has_text, has_passthrough) = span_classes.get(span)?;
                if !has_formula
                    || has_passthrough
                    || !paragraph_owns_walked_span(paragraph, extracted, *span, content_objects)
                {
                    return None;
                }
                if !has_text {
                    continue;
                }
                let glyphs = chars
                    .iter()
                    .filter(|character| {
                        unique_page_content(character, content_objects)
                            .ok()
                            .flatten()
                            .is_some_and(|object_id| span_key(character, object_id) == *span)
                    })
                    .map(|character| formula_glyph_replay(extracted, character))
                    .collect::<Option<Vec<_>>>()?;
                if glyphs.is_empty() {
                    return None;
                }
                split_glyphs.insert(*span, glyphs);
            }
            let metric_bounds = chars
                .iter()
                .map(|character| character.r#box)
                .reduce(Rect::union)?;
            let visual_bounds = chars
                .iter()
                .map(|character| character.visual_bbox)
                .reduce(Rect::union)?;
            let glyph_ink_bounds = chars
                .iter()
                .map(|character| character.visual_bbox)
                .collect();
            let bounds = metric_bounds.union(visual_bounds);
            if bounds.right <= bounds.left + 0.01 {
                return None;
            }
            let source_fonts = chars
                .iter()
                .map(|character| character.font.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let validation_characters = chars
                .iter()
                .map(|character| {
                    let object_id = unique_page_content(character, content_objects)
                        .ok()
                        .flatten()?;
                    let mut expected = formula_validation_character(extracted, character)?;
                    if split_glyphs.contains_key(&span_key(character, object_id)) {
                        expected.baseline_origin =
                            formula_glyph_replay(extracted, character)?.validation_baseline;
                    }
                    Some(expected)
                })
                .collect::<Option<Vec<_>>>()?;
            Some(SourceFormulaUnit {
                chars,
                validation_characters,
                spans,
                split_glyphs,
                vector_paths: Vec::new(),
                inline_images: Vec::new(),
                bounds,
                glyph_ink_bounds,
                ink_bounds: visual_bounds,
                source_fonts,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let source_segments = source_text_segments(paragraph.chars(), content_object_numbers);
    if source_segments.len() != units.len() + 1 {
        return None;
    }
    let formulas = units
        .iter()
        .enumerate()
        .map(|(formula_index, unit)| FormulaContinuityFormula {
            formula_index,
            bounds: unit.bounds,
            line_left: paragraph.bounds.left,
        })
        .collect::<Vec<_>>();
    for attachment in uniquely_attached_source_radicals(&source_segments, &formulas)? {
        let unit = &mut units[attachment.formula_index];
        let radical =
            source_segments[attachment.source_segment_index][attachment.source_character_index];
        let object_id = unique_page_content(radical, content_objects)
            .ok()
            .flatten()?;
        let span = span_key(radical, object_id);
        let span_character_count = paragraph
            .chars()
            .iter()
            .filter(|character| {
                unique_page_content(character, content_objects)
                    .ok()
                    .flatten()
                    .is_some_and(|candidate| span_key(character, candidate) == span)
            })
            .count();
        if span_character_count != 1
            || !paragraph_owns_walked_span(paragraph, extracted, span, content_objects)
        {
            return None;
        }
        unit.chars.insert(0, radical);
        unit.validation_characters
            .insert(0, formula_validation_character(extracted, radical)?);
        unit.spans.push(span);
        unit.spans.sort_unstable();
        unit.spans.dedup();
        unit.bounds = unit.bounds.union(radical.r#box).union(radical.visual_bbox);
        unit.glyph_ink_bounds.insert(0, radical.visual_bbox);
        unit.ink_bounds = unit.ink_bounds.union(radical.visual_bbox);
        unit.source_fonts.push(radical.font.clone());
        unit.source_fonts.sort();
        unit.source_fonts.dedup();
    }
    if attach_relocated_ink {
        attach_uniquely_owned_formula_ink(&mut units, extracted)?;
    }
    Some(units)
}

fn attach_uniquely_owned_formula_ink(
    units: &mut [SourceFormulaUnit<'_>],
    extracted: &ExtractedPage,
) -> Option<()> {
    for path in &extracted.vector_paths {
        let candidates = units
            .iter()
            .enumerate()
            .filter_map(|(index, unit)| formula_owns_vector_path(unit, path).then_some(index))
            .collect::<Vec<_>>();
        let suspicious = units
            .iter()
            .any(|unit| formula_may_own_vector_path(unit, path));
        match candidates.as_slice() {
            [] if !suspicious => continue,
            [index] if path.safe_to_replay => {
                let left = path.start.x.min(path.end.x);
                let right = path.start.x.max(path.end.x);
                let y = (path.start.y + path.end.y) / 2.0;
                let path_bounds = Rect {
                    left,
                    bottom: y - 0.01,
                    right,
                    top: y + 0.01,
                };
                units[*index].vector_paths.push(FormulaVectorReplay {
                    span: (path.content_object, path.byte_start, path.byte_end),
                    content_transform: path.content_transform,
                    bounds: path_bounds,
                });
                units[*index].bounds = units[*index].bounds.union(path_bounds);
                units[*index].ink_bounds = units[*index].ink_bounds.union(path_bounds);
            }
            _ => return None,
        }
    }
    for image in &extracted.inline_images {
        let candidates = units
            .iter()
            .enumerate()
            .filter_map(|(index, unit)| formula_owns_inline_image(unit, image).then_some(index))
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [] => continue,
            [index] => {
                let bounds = walked_image_bounds(image)?;
                units[*index].inline_images.push(FormulaVectorReplay {
                    span: (image.content_object, image.byte_start, image.byte_end),
                    content_transform: image.content_transform,
                    bounds,
                });
                units[*index].bounds = units[*index].bounds.union(bounds);
                units[*index].ink_bounds = units[*index].ink_bounds.union(bounds);
            }
            _ => return None,
        }
    }
    Some(())
}

fn formula_may_own_vector_path(
    unit: &SourceFormulaUnit<'_>,
    path: &crate::walk::WalkedVectorPath,
) -> bool {
    let em = unit
        .chars
        .iter()
        .map(|character| character.font_size)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(0.0_f64, f64::max);
    let left = path.start.x.min(path.end.x);
    let right = path.start.x.max(path.end.x);
    let y = (path.start.y + path.end.y) / 2.0;
    let width = right - left;
    let unit_width = unit.bounds.right - unit.bounds.left;
    let overlap = (right.min(unit.bounds.right) - left.max(unit.bounds.left)).max(0.0);
    em > 0.0
        && path.content_object.0 != 0
        && width > 0.01
        && width <= em * 4.0
        && width <= unit_width + em
        && overlap >= width.min(unit_width) * 0.5
        && y >= unit.bounds.bottom - em * 0.25
        && y <= unit.bounds.top + em * 0.5
}

fn formula_owns_vector_path(
    unit: &SourceFormulaUnit<'_>,
    path: &crate::walk::WalkedVectorPath,
) -> bool {
    if !formula_may_own_vector_path(unit, path) {
        return false;
    }
    let em = unit
        .chars
        .iter()
        .map(|character| character.font_size)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(0.0_f64, f64::max);
    if em <= 0.0 || path.content_object.0 == 0 {
        return false;
    }
    let left = path.start.x.min(path.end.x);
    let right = path.start.x.max(path.end.x);
    let y = (path.start.y + path.end.y) / 2.0;
    let width = right - left;
    let unit_width = unit.bounds.right - unit.bounds.left;
    if width <= 0.01 || width > em * 4.0 || width > unit_width + em {
        return false;
    }
    let highest_baseline = unit
        .chars
        .iter()
        .map(|character| character.baseline_origin.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let caps_formula = y >= highest_baseline - em * 0.05 && y <= unit.bounds.top + em * 0.5;
    let separates_formula_rows = unit
        .chars
        .iter()
        .any(|character| character.baseline_origin.y > y + em * 0.05)
        && unit
            .chars
            .iter()
            .any(|character| character.baseline_origin.y < y - em * 0.05);
    if !caps_formula && !separates_formula_rows {
        return false;
    }
    let overlap = (right.min(unit.bounds.right) - left.max(unit.bounds.left)).max(0.0);
    let comparable_width = width.min(unit_width);
    if comparable_width <= 0.01 || overlap < comparable_width * 0.5 {
        return false;
    }
    let matching_spans = unit
        .spans
        .iter()
        .filter(|span| span.0 == path.content_object)
        .collect::<Vec<_>>();
    let Some(first) = matching_spans.iter().map(|span| span.1).min() else {
        return false;
    };
    let last = matching_spans
        .iter()
        .map(|span| span.2)
        .max()
        .expect("matching formula spans are non-empty");
    let byte_distance = if path.byte_end <= first {
        first - path.byte_end
    } else {
        path.byte_start.saturating_sub(last)
    };
    byte_distance <= 256
}

fn formula_owns_inline_image(
    unit: &SourceFormulaUnit<'_>,
    image: &crate::walk::WalkedInlineImage,
) -> bool {
    if !image.replayable {
        return false;
    }
    let Some(image_bounds) = walked_image_bounds(image) else {
        return false;
    };
    let em = unit
        .chars
        .iter()
        .map(|character| character.font_size)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(0.0_f64, f64::max);
    if em <= 0.0
        || image.content_object.0 == 0
        || image_bounds.right <= image_bounds.left
        || image_bounds.top <= image_bounds.bottom
        || image_bounds.right - image_bounds.left > unit.bounds.right - unit.bounds.left + em
        || image_bounds.top - image_bounds.bottom > unit.bounds.top - unit.bounds.bottom + em
    {
        return false;
    }
    let overlap_x = (image_bounds.right.min(unit.bounds.right)
        - image_bounds.left.max(unit.bounds.left))
    .max(0.0);
    let overlap_y = (image_bounds.top.min(unit.bounds.top)
        - image_bounds.bottom.max(unit.bounds.bottom))
    .max(0.0);
    if overlap_x <= 0.01 || overlap_y <= 0.01 {
        return false;
    }
    let Some(first) = unit
        .spans
        .iter()
        .filter(|span| span.0 == image.content_object)
        .map(|span| span.1)
        .min()
    else {
        return false;
    };
    let last = unit
        .spans
        .iter()
        .filter(|span| span.0 == image.content_object)
        .map(|span| span.2)
        .max()
        .expect("matching formula spans are non-empty");
    let byte_distance = if image.byte_end <= first {
        first - image.byte_end
    } else {
        image.byte_start.saturating_sub(last)
    };
    byte_distance <= 256
}

fn paragraph_owns_walked_span(
    paragraph: &Paragraph,
    extracted: &ExtractedPage,
    span: SpanKey,
    content_objects: &BTreeSet<lopdf::ObjectId>,
) -> bool {
    let paragraph_count = paragraph
        .chars()
        .iter()
        .filter(|character| {
            unique_page_content(character, content_objects)
                .ok()
                .flatten()
                .is_some_and(|object_id| span_key(character, object_id) == span)
        })
        .count();
    let walked_count = extracted
        .walked_characters
        .iter()
        .filter(|character| {
            character.content_object == span.0
                && character.byte_start == span.1
                && character.byte_end == span.2
        })
        .count();
    paragraph_count > 0 && paragraph_count == walked_count
}

fn formula_glyph_replay(extracted: &ExtractedPage, character: &Char) -> Option<FormulaGlyphReplay> {
    let mut matching = extracted.walked_characters.iter().filter(|walked| {
        walked.content_object.0 == character.passthrough.content_object
            && walked.byte_start == character.passthrough.byte_start
            && walked.byte_end == character.passthrough.byte_end
            && walked.code == character.code
            && walked.encoded == character.passthrough.encoded
            && walked.font == character.font
            && point_close(walked.baseline_origin, character.baseline_origin, 1e-7)
    });
    let walked = matching.next()?;
    if matching.next().is_some()
        || walked.encoded.is_empty()
        || walked.source_glyph_scalar_count != 1
    {
        return None;
    }
    Some(FormulaGlyphReplay {
        encoded: walked.encoded.clone(),
        text_matrix: walked.text_matrix_before_glyph,
        validation_baseline: walked.baseline_origin,
        font_resource_name: walked.font.resource_name.clone(),
        font_size: walked.font_size,
    })
}

fn formula_flow_atoms(
    translated_segments: &[Vec<crate::translate::StyledCharacter>],
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Vec<FormulaFlowAtom> {
    let mut atoms = Vec::new();
    for (segment_index, segment) in translated_segments.iter().enumerate() {
        let segment_gap = (segment_index == 0).then_some(section_number_gap).flatten();
        let mut consumed = 0usize;
        for characters in section_aware_styled_text_tokens(segment, segment_gap) {
            let token_end = consumed + characters.len();
            let section_prefix_only = segment_gap.is_some_and(|gap| {
                segment.len() == gap.prefix_glyph_count && token_end == gap.prefix_glyph_count
            });
            let extra_advance_pt = segment_gap
                .filter(|gap| {
                    consumed <= gap.prefix_glyph_count
                        && (token_end > gap.prefix_glyph_count || section_prefix_only)
                })
                .map_or(0.0, |gap| gap.gap_pt);
            atoms.push(FormulaFlowAtom::Text {
                segment_index,
                characters,
                extra_advance_pt,
                section_prefix_only,
            });
            consumed = token_end;
        }
        if segment_index + 1 < translated_segments.len() {
            atoms.push(FormulaFlowAtom::Formula(segment_index));
        }
    }
    atoms
}

fn place_formula_flow(
    atoms: &[FormulaFlowAtom],
    formula_units: &[SourceFormulaUnit<'_>],
    faces: &OutputFontFaces<'_>,
    size: f64,
    slots: &[TypesetLineSlot],
    section_number_gap: Option<PlannedSectionNumberGap>,
    anchor_first_slot: bool,
) -> Option<FormulaFlowAttempt> {
    let mut lines = Vec::<Vec<crate::translate::StyledCharacter>>::new();
    let mut baselines = Vec::<(f64, f64)>::new();
    let mut line_segment_indices = Vec::new();
    let mut formula_relocations = Vec::new();
    let mut formula_line_lefts = Vec::new();
    let mut formula_ink = Vec::new();
    let mut slot_index = 0_usize;
    let Some(first_slot) = slots.first() else {
        return Some(FormulaFlowAttempt::NoFit);
    };
    let mut cursor = first_slot.left;
    let mut open_text_slot = None;
    for (atom_index, atom) in atoms.iter().enumerate() {
        let width = match atom {
            FormulaFlowAtom::Text {
                characters,
                extra_advance_pt,
                ..
            } => styled_token_width(characters, faces, size)? + extra_advance_pt,
            FormulaFlowAtom::Formula(index) => {
                let unit = formula_units.get(*index)?;
                unit.bounds.right - unit.bounds.left
            }
        };
        let mut attached_width = 0.0;
        let mut previous = atom;
        for next in &atoms[atom_index + 1..] {
            if !formula_flow_atoms_must_stay_together(previous, next) {
                break;
            }
            attached_width += formula_flow_atom_width(next, formula_units, faces, size)?;
            previous = next;
        }
        let unbreakable_width = width + attached_width;
        loop {
            let Some(slot) = slots.get(slot_index) else {
                return Some(FormulaFlowAttempt::NoFit);
            };
            if unbreakable_width <= slot.right - cursor + 0.01 {
                break;
            }
            if anchor_first_slot && atom_index == 0 {
                return Some(FormulaFlowAttempt::NoFit);
            }
            slot_index += 1;
            let Some(next_slot) = slots.get(slot_index) else {
                return Some(FormulaFlowAttempt::NoFit);
            };
            cursor = next_slot.left;
            open_text_slot = None;
        }
        let slot = slots[slot_index];
        match atom {
            FormulaFlowAtom::Text {
                segment_index,
                characters,
                ..
            } => {
                if open_text_slot != Some(slot_index) {
                    lines.push(Vec::new());
                    baselines.push((cursor, slot.baseline_y));
                    line_segment_indices.push(*segment_index);
                    open_text_slot = Some(slot_index);
                }
                lines.last_mut()?.extend(characters);
            }
            FormulaFlowAtom::Formula(index) => {
                let unit = &formula_units[*index];
                let delta_x_pt = cursor - unit.bounds.left;
                let target_center_y =
                    slot.baseline_y + (faces.ascent_em() + faces.descent_em()) * size / 2.0;
                let source_center_y = (unit.bounds.bottom + unit.bounds.top) / 2.0;
                let delta_y_pt = target_center_y - source_center_y;
                let characters = unit
                    .validation_characters
                    .iter()
                    .map(|expected| {
                        Some(TypesetCharacter {
                            unicode: expected.unicode,
                            baseline_origin: il::Point {
                                x: expected.baseline_origin.x + delta_x_pt,
                                y: expected.baseline_origin.y + delta_y_pt,
                            },
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                formula_ink.push(translated_rect(unit.ink_bounds, delta_x_pt, delta_y_pt));
                formula_relocations.push(FormulaRelocation {
                    spans: unit.spans.clone(),
                    split_glyphs: unit.split_glyphs.clone(),
                    vector_paths: unit.vector_paths.clone(),
                    inline_images: unit.inline_images.clone(),
                    delta_x_pt,
                    delta_y_pt,
                    characters,
                    text_ink_bounds: unit
                        .glyph_ink_bounds
                        .iter()
                        .copied()
                        .map(|bounds| translated_rect(bounds, delta_x_pt, delta_y_pt))
                        .reduce(Rect::union)?,
                    glyph_ink_bounds: unit
                        .glyph_ink_bounds
                        .iter()
                        .copied()
                        .map(|bounds| translated_rect(bounds, delta_x_pt, delta_y_pt))
                        .collect(),
                    source_fonts: unit.source_fonts.clone(),
                });
                formula_line_lefts.push(slot.left);
                open_text_slot = None;
            }
        }
        cursor += width;
    }
    let mut ink_bounds =
        planned_line_ink_bounds(&lines, &baselines, faces, size, section_number_gap)?;
    ink_bounds.extend(formula_ink);
    Some(FormulaFlowAttempt::Placed(FormulaFlowPlacement {
        lines,
        baselines,
        line_segment_indices,
        formula_relocations,
        formula_line_lefts,
        ink_bounds,
    }))
}

fn formula_flow_atom_width(
    atom: &FormulaFlowAtom,
    formula_units: &[SourceFormulaUnit<'_>],
    faces: &OutputFontFaces<'_>,
    size: f64,
) -> Option<f64> {
    match atom {
        FormulaFlowAtom::Text {
            characters,
            extra_advance_pt,
            ..
        } => Some(styled_token_width(characters, faces, size)? + extra_advance_pt),
        FormulaFlowAtom::Formula(index) => {
            let unit = formula_units.get(*index)?;
            Some(unit.bounds.right - unit.bounds.left)
        }
    }
}

fn formula_flow_atoms_must_stay_together(left: &FormulaFlowAtom, right: &FormulaFlowAtom) -> bool {
    match (left, right) {
        (FormulaFlowAtom::Formula(_), FormulaFlowAtom::Formula(_)) => true,
        (FormulaFlowAtom::Formula(_), FormulaFlowAtom::Text { characters, .. }) => characters
            .iter()
            .find(|character| !character.value.is_whitespace())
            .is_some_and(|character| formula_adjacent_punctuation(character.value)),
        (
            FormulaFlowAtom::Text {
                characters,
                section_prefix_only,
                ..
            },
            FormulaFlowAtom::Formula(_),
        ) => {
            *section_prefix_only
                || characters
                    .iter()
                    .rev()
                    .find(|character| !character.value.is_whitespace())
                    .is_some_and(|character| formula_adjacent_punctuation(character.value))
        }
        _ => false,
    }
}

fn formula_flow_kinsoku_is_unsatisfiable(atoms: &[FormulaFlowAtom]) -> bool {
    let starts_forbidden = match atoms.first() {
        Some(FormulaFlowAtom::Text { characters, .. }) => characters
            .iter()
            .find(|character| !character.value.is_whitespace())
            .is_some_and(|character| mimus_quality_contract::forbidden_line_start(character.value)),
        _ => false,
    };
    let ends_forbidden = match atoms.last() {
        Some(FormulaFlowAtom::Text { characters, .. }) => characters
            .iter()
            .rev()
            .find(|character| !character.value.is_whitespace())
            .is_some_and(|character| mimus_quality_contract::forbidden_line_end(character.value)),
        _ => false,
    };
    starts_forbidden || ends_forbidden
}

fn planned_formula_continuity_text(
    plans: &[(usize, TypesetPlan)],
    output_fonts: &crate::context::OutputFonts,
) -> Option<Vec<FormulaContinuityText>> {
    let faces = OutputFontFaces::parse(output_fonts).ok()?;
    plans
        .iter()
        .map(|(segment_index, plan)| {
            Some(FormulaContinuityText {
                segment_index: *segment_index,
                lines: formula_continuity_lines(
                    &plan.lines,
                    &plan.baselines,
                    &faces,
                    plan.font_size,
                    plan.section_number_gap.filter(|gap| gap.prefix_in_output),
                )?,
            })
        })
        .collect()
}

fn formula_flow_continuity_text(
    placement: &FormulaFlowPlacement,
    faces: &OutputFontFaces<'_>,
    size: f64,
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Option<Vec<FormulaContinuityText>> {
    let lines = formula_continuity_lines(
        &placement.lines,
        &placement.baselines,
        faces,
        size,
        section_number_gap,
    )?;
    if lines.len() != placement.line_segment_indices.len() {
        return None;
    }
    let mut segments = BTreeMap::<usize, Vec<FormulaContinuityLine>>::new();
    for (segment_index, line) in placement.line_segment_indices.iter().copied().zip(lines) {
        segments.entry(segment_index).or_default().push(line);
    }
    Some(
        segments
            .into_iter()
            .map(|(segment_index, lines)| FormulaContinuityText {
                segment_index,
                lines,
            })
            .collect(),
    )
}

fn formula_continuity_lines(
    lines: &[Vec<crate::translate::StyledCharacter>],
    baselines: &[(f64, f64)],
    faces: &OutputFontFaces<'_>,
    size: f64,
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Option<Vec<FormulaContinuityLine>> {
    if lines.len() != baselines.len() {
        return None;
    }
    lines
        .iter()
        .zip(baselines)
        .enumerate()
        .map(|(line_index, (line, &(x, y)))| {
            let width = styled_token_width(line, faces, size)?
                + section_number_gap
                    .filter(|gap| line_index == 0 && line.len() > gap.prefix_glyph_count)
                    .map_or(0.0, |gap| gap.gap_pt);
            let first = line
                .iter()
                .find(|character| !character.value.is_whitespace());
            let last = line
                .iter()
                .rev()
                .find(|character| !character.value.is_whitespace());
            Some(FormulaContinuityLine {
                bounds: Rect {
                    left: x,
                    bottom: y + faces.descent_em() * size,
                    right: x + width,
                    top: y + faces.ascent_em() * size,
                },
                line_left: x,
                starts_with_punctuation: first
                    .is_some_and(|character| formula_adjacent_punctuation(character.value)),
                ends_with_punctuation: last
                    .is_some_and(|character| formula_adjacent_punctuation(character.value)),
            })
        })
        .collect()
}

fn fixed_formula_continuity(
    paragraph: &Paragraph,
    content_objects: &BTreeSet<u32>,
) -> Option<Vec<FormulaContinuityFormula>> {
    let chars = paragraph.chars();
    let mut formulas = raw_fixed_formula_continuity(paragraph, content_objects)?;
    let source_segments = source_text_segments(chars, content_objects);
    if source_segments.len() != formulas.len() + 1 {
        return None;
    }
    for attachment in uniquely_attached_source_radicals(&source_segments, &formulas)? {
        let radical =
            source_segments[attachment.source_segment_index][attachment.source_character_index];
        formulas[attachment.formula_index].bounds = formulas[attachment.formula_index]
            .bounds
            .union(radical.r#box)
            .union(radical.visual_bbox);
    }
    Some(formulas)
}

fn raw_fixed_formula_continuity(
    paragraph: &Paragraph,
    content_objects: &BTreeSet<u32>,
) -> Option<Vec<FormulaContinuityFormula>> {
    let mut formulas = Vec::new();
    let mut start = 0_usize;
    let chars = paragraph.chars();
    while start < chars.len() {
        if prepared_character_class(&chars[start], content_objects)
            != PreparedCharacterClass::Formula
        {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < chars.len()
            && prepared_character_class(&chars[end], content_objects)
                == PreparedCharacterClass::Formula
        {
            end += 1;
        }
        let bounds = chars[start..end]
            .iter()
            .flat_map(|character| [character.r#box, character.visual_bbox])
            .filter(|bounds| rect_is_finite(*bounds))
            .reduce(Rect::union)?;
        formulas.push(FormulaContinuityFormula {
            formula_index: formulas.len(),
            bounds,
            line_left: paragraph.bounds.left,
        });
        start = end;
    }
    Some(formulas)
}

fn normalize_formula_interleaved_punctuation_order(
    paragraph: &Paragraph,
    content_objects: &BTreeSet<u32>,
    source_segments: &mut [Vec<&Char>],
    formulas: &[FormulaContinuityFormula],
    translated_segments: &mut [Vec<crate::translate::StyledCharacter>],
    limit: f64,
) {
    if source_segments.len() != formulas.len() + 1
        || translated_segments.len() != source_segments.len()
        || !limit.is_finite()
        || limit <= 0.0
    {
        return;
    }
    let formula_groups = contiguous_formula_character_groups(paragraph, content_objects);
    for segment_index in 1..formulas.len() {
        let punctuation = source_segments[segment_index]
            .iter()
            .filter_map(|character| character.unicode)
            .filter(|value| !value.is_whitespace())
            .collect::<Vec<_>>();
        if source_segments[segment_index].is_empty() || punctuation.is_empty() {
            continue;
        }
        let Some(bounds) = source_segments[segment_index]
            .iter()
            .flat_map(|character| [character.r#box, character.visual_bbox])
            .filter(|bounds| rect_is_finite(*bounds))
            .reduce(Rect::union)
        else {
            continue;
        };
        let following_formula = formulas[segment_index];
        let segment_follows_formula =
            formula_rects_are_adjacent(following_formula.bounds, bounds, limit);
        let punctuation_only = punctuation
            .iter()
            .all(|value| formula_adjacent_punctuation(*value));
        let short_extraction_inversion = punctuation_only
            && source_segments[segment_index + 1].is_empty()
            && translated_segments[segment_index + 1].is_empty();
        let starts_with_punctuation = source_segments[segment_index]
            .iter()
            .find_map(|character| character.unicode)
            .is_some_and(formula_adjacent_punctuation);
        let shared_model_region = formula_groups
            .get(segment_index - 1)
            .zip(formula_groups.get(segment_index))
            .is_some_and(|(left, right)| formula_groups_share_model_region(left, right));
        let formulas_are_adjacent = formula_groups
            .get(segment_index - 1)
            .zip(formula_groups.get(segment_index))
            .and_then(|(left, right)| {
                Some((
                    left.iter()
                        .map(|character| character.r#box)
                        .reduce(Rect::union)?,
                    right
                        .iter()
                        .map(|character| character.r#box)
                        .reduce(Rect::union)?,
                ))
            })
            .is_some_and(|(left, right)| formula_rects_are_adjacent(left, right, limit));
        let segment_is_on_formula_line = source_segments[segment_index].iter().all(|character| {
            rects_overlap_vertically(character.r#box, following_formula.bounds)
                || rects_overlap_vertically(character.visual_bbox, following_formula.bounds)
        });
        let complete_formula_inversion = starts_with_punctuation
            && shared_model_region
            && formulas_are_adjacent
            && segment_is_on_formula_line;
        if !segment_follows_formula || (!short_extraction_inversion && !complete_formula_inversion)
        {
            continue;
        }
        let mut source = std::mem::take(&mut source_segments[segment_index]);
        source.append(&mut source_segments[segment_index + 1]);
        source_segments[segment_index + 1] = source;
        let mut translated = std::mem::take(&mut translated_segments[segment_index]);
        translated.append(&mut translated_segments[segment_index + 1]);
        translated_segments[segment_index + 1] = translated;
    }
}

fn contiguous_formula_character_groups<'a>(
    paragraph: &'a Paragraph,
    content_objects: &BTreeSet<u32>,
) -> Vec<Vec<&'a Char>> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    for character in paragraph.chars() {
        if prepared_character_class(character, content_objects) == PreparedCharacterClass::Formula {
            current.push(character);
        } else if !current.is_empty() {
            groups.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

fn formula_groups_share_model_region(left: &[&Char], right: &[&Char]) -> bool {
    left.iter().any(|left| {
        left.layout.is_some_and(|left_layout| {
            left_layout.source == LayoutSource::Model
                && left_layout.label == LayoutLabel::InlineFormula
                && right.iter().any(|right| right.layout == Some(left_layout))
        })
    })
}

fn formula_rects_are_adjacent(left: Rect, right: Rect, limit: f64) -> bool {
    mimus_quality_contract::formula_items_are_adjacent(
        left.left,
        left.bottom,
        left.right,
        left.top,
        right.left,
        right.bottom,
        right.right,
        right.top,
        limit,
    )
}

fn rects_overlap_vertically(left: Rect, right: Rect) -> bool {
    left.top > right.bottom + 0.01 && right.top > left.bottom + 0.01
}

fn formula_continuity_limit(paragraph: &Paragraph, content_objects: &BTreeSet<u32>) -> Option<f64> {
    let chars = paragraph.chars();
    let mut word_spacing = chars
        .iter()
        .filter(|character| character.unicode.is_some_and(char::is_whitespace))
        .map(|character| character.r#box.right - character.r#box.left)
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    word_spacing.extend(chars.windows(2).filter_map(|pair| {
        let [left, right] = pair else {
            return None;
        };
        if !right.implicit_space_before
            || !matches!(
                prepared_character_class(left, content_objects),
                PreparedCharacterClass::Text { .. }
            )
            || !matches!(
                prepared_character_class(right, content_objects),
                PreparedCharacterClass::Text { .. }
            )
            || (left.baseline_origin.y - right.baseline_origin.y).abs()
                > left.font_size.max(right.font_size) * 0.35
        {
            return None;
        }
        let gap = right.r#box.left - left.r#box.right;
        (gap.is_finite() && gap > 0.0).then_some(gap)
    }));
    mimus_quality_contract::formula_continuity_limit(
        word_spacing,
        chars.iter().map(|character| character.font_size),
    )
}

#[derive(Clone, Copy)]
struct FormulaContinuityItem {
    is_formula: bool,
    bounds: Rect,
    line_left: f64,
    starts_with_punctuation: bool,
    ends_with_punctuation: bool,
}

fn formula_continuity_is_valid(
    translated_segments: &[Vec<crate::translate::StyledCharacter>],
    text: &[FormulaContinuityText],
    formulas: &[FormulaContinuityFormula],
    limit: f64,
) -> bool {
    if formulas.len() + 1 != translated_segments.len()
        || !limit.is_finite()
        || limit <= 0.0
        || formulas
            .iter()
            .enumerate()
            .any(|(index, formula)| formula.formula_index != index)
    {
        return false;
    }
    let mut text_by_segment = BTreeMap::new();
    for placement in text {
        if placement.segment_index >= translated_segments.len()
            || text_by_segment
                .insert(placement.segment_index, placement)
                .is_some()
        {
            return false;
        }
    }
    let mut items = Vec::new();
    for segment_index in 0..translated_segments.len() {
        if let Some(placement) = text_by_segment.get(&segment_index) {
            items.extend(placement.lines.iter().map(|line| FormulaContinuityItem {
                is_formula: false,
                bounds: line.bounds,
                line_left: line.line_left,
                starts_with_punctuation: line.starts_with_punctuation,
                ends_with_punctuation: line.ends_with_punctuation,
            }));
        }
        if let Some(formula) = formulas.get(segment_index) {
            items.push(FormulaContinuityItem {
                is_formula: true,
                bounds: formula.bounds,
                line_left: formula.line_left,
                starts_with_punctuation: false,
                ends_with_punctuation: false,
            });
        }
    }
    if items
        .first()
        .is_some_and(|item| item.is_formula && item.bounds.left - item.line_left > limit + 0.01)
    {
        return false;
    }
    items.windows(2).all(|pair| {
        let [left, right] = pair else {
            return false;
        };
        if !left.is_formula && !right.is_formula {
            return true;
        }
        let same_line = mimus_quality_contract::formula_items_share_line(
            left.bounds.bottom,
            left.bounds.top,
            right.bounds.bottom,
            right.bounds.top,
        );
        if (left.ends_with_punctuation || right.starts_with_punctuation) && !same_line {
            return false;
        }
        if same_line {
            let gap = right.bounds.left - left.bounds.right;
            gap >= -0.01 && gap <= limit + 0.01
        } else {
            let left_center = (left.bounds.bottom + left.bounds.top) / 2.0;
            let right_center = (right.bounds.bottom + right.bounds.top) / 2.0;
            left_center > right_center + 0.01 && right.bounds.left - right.line_left <= limit + 0.01
        }
    })
}

fn formula_adjacent_punctuation(value: char) -> bool {
    value.is_ascii_punctuation()
        || matches!(
            value,
            '\u{3001}'
                | '\u{3002}'
                | '\u{3008}'..='\u{3011}'
                | '\u{3014}'..='\u{301f}'
                | '\u{ff01}'
                | '\u{ff08}'
                | '\u{ff09}'
                | '\u{ff0c}'
                | '\u{ff0e}'
                | '\u{ff1a}'
                | '\u{ff1b}'
                | '\u{ff1f}'
        )
}

fn formula_validation_character(
    extracted: &ExtractedPage,
    character: &Char,
) -> Option<TypesetCharacter> {
    let walk_index = extracted
        .walked_characters
        .iter()
        .enumerate()
        .find(|(_, walked)| {
            walked.content_object.0 == character.passthrough.content_object
                && walked.byte_start == character.passthrough.byte_start
                && walked.byte_end == character.passthrough.byte_end
                && walked.code == character.code
                && point_close(walked.baseline_origin, character.baseline_origin, 1e-7)
        })?
        .0;
    let engine_index = extracted
        .character_alignment
        .engine_indices_by_walk
        .get(walk_index)
        .copied()
        .flatten()
        .or_else(|| {
            sequence_engine_indices_by_walk(
                &extracted.walked_characters,
                &extracted.engine_characters,
            )
            .and_then(|indices| indices.get(walk_index).copied().flatten())
        })
        .or_else(|| {
            let expected = ExpectedOutputCharacter {
                unicode: character.unicode,
                baseline_origin: character.baseline_origin,
            };
            match_output_character(
                &expected,
                &extracted.engine_characters,
                &vec![false; extracted.engine_characters.len()],
                0.25,
            )
            .and_then(|indices| indices.into_iter().next())
        });
    if let Some(engine) =
        engine_index.and_then(|engine_index| extracted.engine_characters.get(engine_index))
    {
        return Some(TypesetCharacter {
            unicode: engine.unicode.or(character.unicode)?,
            baseline_origin: engine.baseline_origin,
        });
    }
    if extracted
        .engine_characters
        .iter()
        .any(|engine| point_close(character.baseline_origin, engine.baseline_origin, 0.25))
    {
        return None;
    }
    Some(TypesetCharacter {
        unicode: character.unicode?,
        baseline_origin: character.baseline_origin,
    })
}

fn styled_token_width(
    token: &[crate::translate::StyledCharacter],
    faces: &OutputFontFaces<'_>,
    size: f64,
) -> Option<f64> {
    token.iter().try_fold(0.0, |sum, character| {
        let face = faces.face_for(*character)?;
        let glyph = face.glyph_index(character.value)?;
        Some(sum + glyph_advance_em(face, glyph)? * size)
    })
}

fn planned_section_number_gap(
    translated: &[crate::translate::StyledCharacter],
    faces: &OutputFontFaces<'_>,
    size: f64,
    source: SourceSectionNumberGeometry,
) -> Option<PlannedSectionNumberGap> {
    if source.prefix_glyph_count == 0
        || (source.prefix_in_output
            && (translated.is_empty() || source.prefix_glyph_count > translated.len()))
    {
        return None;
    }
    let output_prefix_width = if source.prefix_in_output {
        styled_token_width(&translated[..source.prefix_glyph_count], faces, size)?
    } else {
        source.source_prefix_right - source.source_prefix_left
    };
    let position = mimus_quality_contract::retained_section_number_position(
        source.source_prefix_left,
        source.source_title_left,
        output_prefix_width,
        size,
    )?;
    Some(PlannedSectionNumberGap {
        prefix_glyph_count: source.prefix_glyph_count,
        prefix_in_output: source.prefix_in_output,
        source_prefix_left: source.source_prefix_left,
        source_title_left: source.source_title_left,
        output_prefix_width,
        output_title_left: position.title_left,
        gap_pt: position.gap_pt,
        clamped: position.clamped,
    })
}

fn section_number_title_is_formula_first(
    translated_segments: &[Vec<crate::translate::StyledCharacter>],
    source: SourceSectionNumberGeometry,
) -> bool {
    translated_segments.first().is_some_and(|segment| {
        let prefix_glyph_count = if source.prefix_in_output {
            source.prefix_glyph_count
        } else {
            0
        };
        segment.get(prefix_glyph_count..).is_some_and(|title| {
            title
                .iter()
                .all(|character| character.value.is_whitespace())
        })
    })
}

fn translated_rect(rect: Rect, delta_x: f64, delta_y: f64) -> Rect {
    Rect {
        left: rect.left + delta_x,
        bottom: rect.bottom + delta_y,
        right: rect.right + delta_x,
        top: rect.top + delta_y,
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_text_segment<'a>(
    chars: &[&Char],
    translated: &[crate::translate::StyledCharacter],
    content_objects: &BTreeSet<lopdf::ObjectId>,
    output_fonts: &'a crate::context::OutputFonts,
    page_bounds: Rect,
    obstacles: &[Rect],
    line_slots: Option<&[TypesetLineSlot]>,
    first_line_indent: Option<f64>,
    section_number_geometry: Option<SourceSectionNumberGeometry>,
) -> std::result::Result<TypesetPlan, TypesetPlanError<'a>> {
    if chars.is_empty() {
        return Err(TypesetPlanError::Preserved(
            il::PreservedReason::TypesetProtocol,
        ));
    }
    let mut spans = chars
        .iter()
        .filter_map(|character| {
            unique_page_content(character, content_objects)
                .ok()
                .flatten()
                .map(|id| span_key(character, id))
        })
        .collect::<Vec<_>>();
    spans.sort_unstable();
    spans.dedup();
    if spans.is_empty() {
        return Err(TypesetPlanError::Preserved(
            il::PreservedReason::Unlocatable,
        ));
    }
    let translated = normalize_typeset_whitespace(translated);
    if translated.is_empty() {
        return Ok(TypesetPlan {
            spans,
            lines: Vec::new(),
            baselines: Vec::new(),
            section_number_gap: None,
            formula_relocations: Vec::new(),
            text_vector_relocations: Vec::new(),
            ink_bounds: Vec::new(),
            font_size: chars[0].font_size.max(MIN_FONT_SIZE_PT),
            single_line_expansion: None,
            multi_line_expansion: None,
        });
    }
    let faces = OutputFontFaces::parse(output_fonts)
        .map_err(|_| TypesetPlanError::Preserved(il::PreservedReason::UnsupportedFont))?;
    for is_bold in [false, true] {
        let missing_characters = translated
            .iter()
            .filter(|character| character.bold == is_bold)
            .map(|character| character.value)
            .filter(|value| faces.key_for(*value, is_bold).is_none())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(16)
            .collect::<String>();
        if !missing_characters.is_empty() {
            return Err(TypesetPlanError::MissingGlyphs {
                missing_characters,
                primary_font: if is_bold {
                    &output_fonts.bold
                } else {
                    &output_fonts.regular
                },
                fallback_font: if is_bold {
                    &output_fonts.fallback_bold
                } else {
                    &output_fonts.fallback_regular
                },
            });
        }
    }
    let container = typeset_container(chars.iter().copied()).ok_or(TypesetPlanError::Preserved(
        il::PreservedReason::TypesetOverflow,
    ))?;
    let preferred = preferred_typeset_font_size(chars).unwrap_or(chars[0].font_size);
    let first = chars[0];
    let source_is_single_line = source_is_single_line(chars);
    let mut size = preferred.max(MIN_FONT_SIZE_PT);
    loop {
        let section_number_gap = section_number_geometry
            .map(|source| {
                planned_section_number_gap(&translated, &faces, size, source).ok_or(
                    TypesetPlanError::Preserved(il::PreservedReason::TypesetProtocol),
                )
            })
            .transpose()?;
        let inline_section_number_gap = section_number_gap.filter(|gap| gap.prefix_in_output);
        let single_line_start_x = section_number_gap.map_or_else(
            || first.baseline_origin.x.max(container.left),
            |gap| {
                if gap.prefix_in_output {
                    gap.source_prefix_left
                } else {
                    gap.output_title_left
                }
            },
        );
        let effective_first_line_indent = section_number_gap.map_or_else(
            || first_line_indent.unwrap_or(0.0),
            |_| (single_line_start_x - container.left).max(0.0),
        );
        if let Some(slots) = line_slots
            && let Some(slotted_lines) = wrap_styled_text_in_slots(
                &translated,
                &faces,
                size,
                slots,
                inline_section_number_gap,
            )
        {
            let lines = slotted_lines
                .iter()
                .map(|(_, line)| line.clone())
                .collect::<Vec<_>>();
            let baselines = slotted_lines
                .iter()
                .enumerate()
                .map(|(line_index, (slot_index, _))| {
                    let slot = slots[*slot_index];
                    (
                        if line_index == 0 {
                            section_number_gap.map_or(slot.left, |_| single_line_start_x)
                        } else {
                            slot.left
                        },
                        slot.baseline_y,
                    )
                })
                .collect::<Vec<_>>();
            if let Some(ink_bounds) =
                planned_line_ink_bounds(&lines, &baselines, &faces, size, inline_section_number_gap)
                && ink_bounds
                    .iter()
                    .zip(&slotted_lines)
                    .all(|(ink, (slot_index, _))| {
                        let slot = slots[*slot_index];
                        ink.left >= slot.left - 0.01 && ink.right <= slot.right + 0.01
                    })
                && ink_bounds_are_safe(&ink_bounds, page_bounds, obstacles)
            {
                return Ok(TypesetPlan {
                    spans,
                    lines,
                    baselines,
                    section_number_gap,
                    formula_relocations: Vec::new(),
                    text_vector_relocations: Vec::new(),
                    ink_bounds,
                    font_size: size,
                    single_line_expansion: None,
                    multi_line_expansion: None,
                });
            }
        }
        if line_slots.is_none()
            && let Some(lines) = wrap_styled_text(
                &translated,
                &faces,
                size,
                container.right - container.left,
                effective_first_line_indent,
                inline_section_number_gap,
            )
        {
            if source_is_single_line
                && lines.len() == 1
                && let Some(fit) = single_line_ink_fit(
                    &lines[0],
                    &faces,
                    size,
                    single_line_start_x,
                    first.baseline_origin.y,
                    container,
                    page_bounds,
                    obstacles,
                    inline_section_number_gap,
                )
            {
                return Ok(TypesetPlan {
                    spans,
                    lines,
                    baselines: vec![(single_line_start_x, fit.baseline_y)],
                    section_number_gap,
                    formula_relocations: Vec::new(),
                    text_vector_relocations: Vec::new(),
                    ink_bounds: vec![fit.ink_bounds],
                    font_size: size,
                    single_line_expansion: fit.expansion,
                    multi_line_expansion: None,
                });
            }
            let ascent = faces.ascent_em() * size;
            let descent = faces.descent_em() * size;
            let first_y = first.baseline_origin.y.min(container.top - ascent);
            let baselines = lines
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    (
                        if index == 0 {
                            container.left + effective_first_line_indent
                        } else {
                            container.left
                        },
                        first_y - index as f64 * size * LINE_ADVANCE_EM,
                    )
                })
                .collect::<Vec<_>>();
            let last_y = baselines.last().unwrap().1;
            if let Some(ink_bounds) =
                planned_line_ink_bounds(&lines, &baselines, &faces, size, inline_section_number_gap)
                && ink_bounds_are_safe(&ink_bounds, page_bounds, obstacles)
            {
                let overflow_top = (first_y + ascent - container.top).max(0.0);
                let overflow_bottom = (container.bottom - (last_y + descent)).max(0.0);
                let fits_container = overflow_top <= 0.01 && overflow_bottom <= 0.01;
                let may_expand = !source_is_single_line && size <= MIN_FONT_SIZE_PT + 0.001;
                if fits_container || may_expand {
                    return Ok(TypesetPlan {
                        spans,
                        lines,
                        baselines,
                        section_number_gap,
                        formula_relocations: Vec::new(),
                        text_vector_relocations: Vec::new(),
                        ink_bounds,
                        font_size: size,
                        single_line_expansion: None,
                        multi_line_expansion: (!fits_container).then_some(
                            MultiLineBoundsExpansion {
                                top_pt: overflow_top,
                                bottom_pt: overflow_bottom,
                            },
                        ),
                    });
                }
            }
            if !source_is_single_line && size <= MIN_FONT_SIZE_PT + 0.001 {
                let slots = obstacle_aware_multiline_slots(
                    container,
                    first_y,
                    ascent,
                    descent,
                    size,
                    page_bounds,
                    obstacles,
                    Some(effective_first_line_indent),
                );
                if let Some(slotted_lines) = wrap_styled_text_in_slots(
                    &translated,
                    &faces,
                    size,
                    &slots,
                    inline_section_number_gap,
                ) {
                    let lines = slotted_lines
                        .iter()
                        .map(|(_, line)| line.clone())
                        .collect::<Vec<_>>();
                    let baselines = slotted_lines
                        .iter()
                        .enumerate()
                        .map(|(line_index, (slot_index, _))| {
                            let slot = slots[*slot_index];
                            (
                                if line_index == 0 {
                                    section_number_gap.map_or(slot.left, |_| single_line_start_x)
                                } else {
                                    slot.left
                                },
                                slot.baseline_y,
                            )
                        })
                        .collect::<Vec<_>>();
                    if let Some(ink_bounds) = planned_line_ink_bounds(
                        &lines,
                        &baselines,
                        &faces,
                        size,
                        inline_section_number_gap,
                    ) && ink_bounds_are_safe(&ink_bounds, page_bounds, obstacles)
                    {
                        let first_baseline = baselines.first().unwrap().1;
                        let last_baseline = baselines.last().unwrap().1;
                        let overflow_top = (first_baseline + ascent - container.top).max(0.0);
                        let overflow_bottom =
                            (container.bottom - (last_baseline + descent)).max(0.0);
                        return Ok(TypesetPlan {
                            spans,
                            lines,
                            baselines,
                            section_number_gap,
                            formula_relocations: Vec::new(),
                            text_vector_relocations: Vec::new(),
                            ink_bounds,
                            font_size: size,
                            single_line_expansion: None,
                            multi_line_expansion: (overflow_top > 0.01 || overflow_bottom > 0.01)
                                .then_some(MultiLineBoundsExpansion {
                                    top_pt: overflow_top,
                                    bottom_pt: overflow_bottom,
                                }),
                        });
                    }
                }
            }
        }
        if size <= MIN_FONT_SIZE_PT + 0.001 {
            break;
        }
        size = (size - 0.5).max(MIN_FONT_SIZE_PT);
    }
    Err(TypesetPlanError::Preserved(
        il::PreservedReason::TypesetOverflow,
    ))
}

fn planned_line_ink_bounds(
    lines: &[Vec<crate::translate::StyledCharacter>],
    baselines: &[(f64, f64)],
    faces: &OutputFontFaces<'_>,
    size: f64,
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Option<Vec<Rect>> {
    lines
        .iter()
        .zip(baselines)
        .enumerate()
        .map(|(line_index, (line, &(x, y)))| {
            styled_line_ink_bounds_with_gap(
                line,
                faces,
                size,
                x,
                y,
                (line_index == 0).then_some(section_number_gap).flatten(),
            )
        })
        .collect()
}

fn ink_bounds_are_safe(ink_bounds: &[Rect], page_bounds: Rect, obstacles: &[Rect]) -> bool {
    ink_bounds.iter().all(|ink| {
        rect_contains(page_bounds, *ink, 0.01)
            && obstacles
                .iter()
                .all(|obstacle| intersection_area(*ink, *obstacle) <= 0.0001)
    }) && !rects_intersect_each_other(ink_bounds)
}

#[allow(clippy::too_many_arguments)]
fn obstacle_aware_multiline_slots(
    container: Rect,
    first_baseline_y: f64,
    ascent: f64,
    descent: f64,
    size: f64,
    page_bounds: Rect,
    obstacles: &[Rect],
    first_line_indent: Option<f64>,
) -> Vec<TypesetLineSlot> {
    let left = container.left.max(page_bounds.left);
    let right = container.right.min(page_bounds.right);
    if right <= left + 0.01 {
        return Vec::new();
    }
    let mut slots = Vec::new();
    let mut baseline_y = first_baseline_y;
    while baseline_y + descent >= page_bounds.bottom - 0.01 {
        let line_bottom = baseline_y + descent;
        let line_top = baseline_y + ascent;
        let mut gaps = vec![(left, right)];
        for obstacle in obstacles.iter().filter(|obstacle| {
            obstacle.top > line_bottom + 0.01 && obstacle.bottom < line_top - 0.01
        }) {
            let mut remaining = Vec::new();
            for (gap_left, gap_right) in gaps {
                if obstacle.right <= gap_left + 0.01 || obstacle.left >= gap_right - 0.01 {
                    remaining.push((gap_left, gap_right));
                    continue;
                }
                if obstacle.left > gap_left + 0.01 {
                    remaining.push((gap_left, obstacle.left.min(gap_right)));
                }
                if obstacle.right < gap_right - 0.01 {
                    remaining.push((obstacle.right.max(gap_left), gap_right));
                }
            }
            gaps = remaining;
            if gaps.is_empty() {
                break;
            }
        }
        if gaps.is_empty() {
            break;
        }
        slots.extend(
            gaps.into_iter()
                .filter(|(gap_left, gap_right)| gap_right > &(gap_left + 0.01))
                .map(|(gap_left, gap_right)| TypesetLineSlot {
                    left: gap_left,
                    right: gap_right,
                    baseline_y,
                }),
        );
        baseline_y -= size * LINE_ADVANCE_EM;
    }
    if let Some(indent) = first_line_indent.filter(|indent| *indent > 0.01) {
        let first_line_x = container.left + indent;
        if first_line_x < page_bounds.left - 0.01 || first_line_x >= right - 0.01 {
            return Vec::new();
        }
        let Some(slot_index) = slots.iter().position(|slot| {
            (slot.baseline_y - first_baseline_y).abs() <= 0.01
                && slot.left <= first_line_x + 0.01
                && slot.right > first_line_x + 0.01
        }) else {
            return Vec::new();
        };
        slots.drain(..slot_index);
        slots[0].left = first_line_x;
    }
    slots
}

fn rects_intersect_each_other(rects: &[Rect]) -> bool {
    rects.iter().enumerate().any(|(index, left)| {
        rects[index + 1..]
            .iter()
            .any(|right| intersection_area(*left, *right) > 0.0001)
    })
}

fn source_is_single_line(chars: &[&Char]) -> bool {
    let baseline = chars[0].baseline_origin.y;
    chars
        .iter()
        .all(|character| (character.baseline_origin.y - baseline).abs() <= 0.01)
}

#[allow(clippy::too_many_arguments)]
fn single_line_ink_fit(
    line: &[crate::translate::StyledCharacter],
    faces: &OutputFontFaces<'_>,
    size: f64,
    start_x: f64,
    baseline_y: f64,
    container: Rect,
    page_bounds: Rect,
    obstacles: &[Rect],
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Option<SingleLineInkFit> {
    let ink = styled_line_ink_bounds_with_gap(
        line,
        faces,
        size,
        start_x,
        baseline_y,
        section_number_gap,
    )?;
    if ink.left < container.left - 0.01 || ink.right > container.right + 0.01 {
        return None;
    }
    let top_pt = (ink.top - container.top).max(0.0);
    let bottom_pt = (container.bottom - ink.bottom).max(0.0);
    if line.iter().any(|character| character.bold) && (top_pt > 0.01 || bottom_pt > 0.01) {
        let delta_y = if top_pt > 0.01 && bottom_pt <= 0.01 {
            -top_pt
        } else if bottom_pt > 0.01 && top_pt <= 0.01 {
            bottom_pt
        } else {
            0.0
        };
        let shifted = translated_rect(ink, 0.0, delta_y);
        if delta_y != 0.0
            && rect_contains(container, shifted, 0.01)
            && rect_contains(page_bounds, shifted, 0.01)
            && obstacles
                .iter()
                .all(|obstacle| intersection_area(shifted, *obstacle) <= 0.0001)
        {
            return Some(SingleLineInkFit {
                baseline_y: baseline_y + delta_y,
                ink_bounds: shifted,
                expansion: None,
            });
        }
    }
    let allowance =
        (size * SINGLE_LINE_MAX_VERTICAL_OVERFLOW_EM).min(SINGLE_LINE_MAX_VERTICAL_OVERFLOW_PT);
    if top_pt > allowance + 0.01 || bottom_pt > allowance + 0.01 {
        return None;
    }
    if !rect_contains(page_bounds, ink, 0.01)
        || obstacles
            .iter()
            .any(|obstacle| intersection_area(ink, *obstacle) > 0.0001)
    {
        return None;
    }
    Some(SingleLineInkFit {
        baseline_y,
        ink_bounds: ink,
        expansion: (top_pt > 0.01 || bottom_pt > 0.01)
            .then_some(SingleLineBoundsExpansion { top_pt, bottom_pt }),
    })
}

#[cfg(test)]
fn styled_line_ink_bounds(
    line: &[crate::translate::StyledCharacter],
    faces: &OutputFontFaces<'_>,
    size: f64,
    start_x: f64,
    baseline_y: f64,
) -> Option<Rect> {
    styled_line_ink_bounds_with_gap(line, faces, size, start_x, baseline_y, None)
}

fn styled_line_ink_bounds_with_gap(
    line: &[crate::translate::StyledCharacter],
    faces: &OutputFontFaces<'_>,
    size: f64,
    start_x: f64,
    baseline_y: f64,
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Option<Rect> {
    let mut x = start_x;
    let mut ink = None;
    for (index, character) in line.iter().enumerate() {
        if section_number_gap.is_some_and(|gap| index == gap.prefix_glyph_count) {
            x += section_number_gap.unwrap().gap_pt;
        }
        let face = faces.face_for(*character)?;
        let glyph = face.glyph_index(character.value)?;
        let scale = size / f64::from(face.units_per_em());
        if let Some(bounds) = face.glyph_bounding_box(glyph) {
            let bounds = Rect {
                left: x + f64::from(bounds.x_min) * scale,
                bottom: baseline_y + f64::from(bounds.y_min) * scale,
                right: x + f64::from(bounds.x_max) * scale,
                top: baseline_y + f64::from(bounds.y_max) * scale,
            };
            ink = Some(ink.map_or(bounds, |current: Rect| current.union(bounds)));
        }
        x += glyph_advance_em(face, glyph)? * size;
    }
    ink
}

fn rect_contains(outer: Rect, inner: Rect, tolerance: f64) -> bool {
    inner.left >= outer.left - tolerance
        && inner.bottom >= outer.bottom - tolerance
        && inner.right <= outer.right + tolerance
        && inner.top <= outer.top + tolerance
}

fn normalize_typeset_whitespace(
    text: &[crate::translate::StyledCharacter],
) -> Vec<crate::translate::StyledCharacter> {
    let mut normalized = Vec::with_capacity(text.len());
    for &character in text {
        if character.value.is_whitespace() {
            if !normalized.is_empty()
                && !normalized
                    .last()
                    .is_some_and(|previous: &crate::translate::StyledCharacter| {
                        previous.value == ' '
                    })
            {
                normalized.push(crate::translate::StyledCharacter {
                    value: ' ',
                    bold: character.bold,
                });
            }
        } else {
            normalized.push(character);
        }
    }
    if normalized
        .last()
        .is_some_and(|character| character.value == ' ')
    {
        normalized.pop();
    }
    normalized
}

fn wrap_styled_text(
    text: &[crate::translate::StyledCharacter],
    faces: &OutputFontFaces<'_>,
    size: f64,
    width: f64,
    first_line_indent: f64,
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Option<Vec<Vec<crate::translate::StyledCharacter>>> {
    if !styled_text_kinsoku_is_satisfiable(text) {
        return None;
    }
    let first_line_width = width - first_line_indent.max(0.0);
    if !first_line_width.is_finite() || first_line_width <= 0.01 {
        return None;
    }
    let mut lines = vec![Vec::new()];
    let mut line_width = 0.0;
    let mut consumed = 0usize;
    for token in section_aware_styled_text_tokens(text, section_number_gap) {
        let token_end = consumed + token.len();
        let mut token_width = token.iter().try_fold(0.0, |sum, character| {
            let face = faces.face_for(*character)?;
            let glyph = face.glyph_index(character.value)?;
            Some(sum + glyph_advance_em(face, glyph)? * size)
        })?;
        if section_number_gap.is_some_and(|gap| {
            consumed <= gap.prefix_glyph_count && token_end > gap.prefix_glyph_count
        }) {
            token_width += section_number_gap.unwrap().gap_pt;
        }
        let line_limit = if lines.len() == 1 {
            first_line_width
        } else {
            width
        };
        if token_width > line_limit + 0.01 && line_width <= 0.0 {
            return None;
        }
        if line_width > 0.0 && line_width + token_width > line_limit + 0.01 {
            lines.push(Vec::new());
            line_width = 0.0;
            if token_width > width + 0.01 {
                return None;
            }
        }
        lines.last_mut().unwrap().extend(token);
        line_width += token_width;
        consumed = token_end;
    }
    Some(lines)
}

fn preferred_body_font_size(chars: &[&Char]) -> Option<f64> {
    let (body, ordinary) = body_and_ordinary_font_characters(chars);
    font_size_mode(&ordinary).or_else(|| font_size_mode(&body))
}

fn preferred_typeset_font_size(chars: &[&Char]) -> Option<f64> {
    let legacy_mean = chars
        .iter()
        .map(|character| character.font_size)
        .sum::<f64>()
        / chars.len() as f64;
    let (body, ordinary) = body_and_ordinary_font_characters(chars);
    let script_count = body.len().saturating_sub(ordinary.len());
    if script_count <= ordinary.len() {
        return (legacy_mean.is_finite() && legacy_mean > 0.0).then_some(legacy_mean);
    }
    preferred_body_font_size(chars)
        .or_else(|| (legacy_mean.is_finite() && legacy_mean > 0.0).then_some(legacy_mean))
}

fn body_and_ordinary_font_characters<'a>(chars: &[&'a Char]) -> (Vec<&'a Char>, Vec<&'a Char>) {
    let body = chars
        .iter()
        .copied()
        .filter(|character| {
            character.visible
                && character.font_size.is_finite()
                && character.font_size > 0.0
                && character
                    .unicode
                    .is_some_and(|unicode| !unicode.is_whitespace())
                && character
                    .layout
                    .is_some_and(|layout| layout.policy == TranslationPolicy::Translate)
        })
        .collect::<Vec<_>>();
    let ordinary = body
        .iter()
        .copied()
        .filter(|candidate| {
            !body.iter().copied().any(|anchor| {
                !std::ptr::eq(anchor, *candidate) && geometrically_proves_script(anchor, candidate)
            })
        })
        .collect();
    (body, ordinary)
}

fn font_size_mode(chars: &[&Char]) -> Option<f64> {
    let mut counts = Vec::<(f64, usize)>::new();
    for candidate in chars {
        if let Some((_, count)) = counts
            .iter_mut()
            .find(|(size, _)| size.to_bits() == candidate.font_size.to_bits())
        {
            *count += 1;
        } else {
            counts.push((candidate.font_size, 1));
        }
    }
    counts
        .into_iter()
        .max_by(|(left_size, left_count), (right_size, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| left_size.total_cmp(right_size))
        })
        .map(|(size, _)| size)
}

fn wrap_styled_text_in_slots(
    text: &[crate::translate::StyledCharacter],
    faces: &OutputFontFaces<'_>,
    size: f64,
    slots: &[TypesetLineSlot],
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Option<Vec<(usize, Vec<crate::translate::StyledCharacter>)>> {
    if !styled_text_kinsoku_is_satisfiable(text) {
        return None;
    }
    let mut lines = Vec::<(usize, Vec<crate::translate::StyledCharacter>)>::new();
    let mut slot_index = 0_usize;
    let mut line_width = 0.0;
    let mut consumed = 0usize;
    for token in section_aware_styled_text_tokens(text, section_number_gap) {
        let token_end = consumed + token.len();
        let mut token_width = token.iter().try_fold(0.0, |sum, character| {
            let face = faces.face_for(*character)?;
            let glyph = face.glyph_index(character.value)?;
            Some(sum + glyph_advance_em(face, glyph)? * size)
        })?;
        if section_number_gap.is_some_and(|gap| {
            consumed <= gap.prefix_glyph_count && token_end > gap.prefix_glyph_count
        }) {
            token_width += section_number_gap.unwrap().gap_pt;
        }
        loop {
            let slot = slots.get(slot_index)?;
            let slot_width = slot.right - slot.left;
            if token_width > slot_width + 0.01 {
                if line_width > 0.0 {
                    slot_index += 1;
                    line_width = 0.0;
                    continue;
                }
                slot_index += 1;
                continue;
            }
            if line_width > 0.0 && line_width + token_width > slot_width + 0.01 {
                slot_index += 1;
                line_width = 0.0;
                continue;
            }
            if lines.last().is_none_or(|(index, _)| *index != slot_index) {
                lines.push((slot_index, Vec::new()));
            }
            lines.last_mut().unwrap().1.extend(token);
            line_width += token_width;
            consumed = token_end;
            break;
        }
    }
    Some(lines)
}

fn section_aware_styled_text_tokens(
    text: &[crate::translate::StyledCharacter],
    section_number_gap: Option<PlannedSectionNumberGap>,
) -> Vec<Vec<crate::translate::StyledCharacter>> {
    let mut tokens = styled_text_tokens(text);
    let Some(gap) = section_number_gap else {
        return tokens;
    };
    let mut consumed = 0usize;
    let Some(title_token_index) = tokens.iter().position(|token| {
        consumed += token.len();
        consumed > gap.prefix_glyph_count
    }) else {
        return tokens;
    };
    if title_token_index > 0 {
        let joined = tokens
            .drain(..=title_token_index)
            .flatten()
            .collect::<Vec<_>>();
        tokens.insert(0, joined);
    }
    tokens
}

fn styled_text_tokens(
    text: &[crate::translate::StyledCharacter],
) -> Vec<Vec<crate::translate::StyledCharacter>> {
    let mut tokens = Vec::<Vec<crate::translate::StyledCharacter>>::new();
    for (index, &character) in text.iter().enumerate() {
        let joins_ascii_word = character.value.is_ascii_alphanumeric()
            && index
                .checked_sub(1)
                .and_then(|previous| text.get(previous))
                .is_some_and(|previous| previous.value.is_ascii_alphanumeric());
        let previous_nonspace = text[..index]
            .iter()
            .rev()
            .find(|candidate| !candidate.value.is_whitespace());
        let next_nonspace = text[index..]
            .iter()
            .find(|candidate| !candidate.value.is_whitespace());
        let joins_kinsoku_group = !tokens.is_empty()
            && (next_nonspace.is_some_and(|candidate| {
                mimus_quality_contract::forbidden_line_start(candidate.value)
            }) || previous_nonspace.is_some_and(|candidate| {
                mimus_quality_contract::forbidden_line_end(candidate.value)
            }));
        if joins_ascii_word || joins_kinsoku_group {
            tokens.last_mut().unwrap().push(character);
        } else {
            tokens.push(vec![character]);
        }
    }
    tokens
}

fn styled_text_kinsoku_is_satisfiable(text: &[crate::translate::StyledCharacter]) -> bool {
    let first = text
        .iter()
        .find(|character| !character.value.is_whitespace());
    let last = text
        .iter()
        .rev()
        .find(|character| !character.value.is_whitespace());
    !first.is_some_and(|character| mimus_quality_contract::forbidden_line_start(character.value))
        && !last
            .is_some_and(|character| mimus_quality_contract::forbidden_line_end(character.value))
}

fn glyph_advance_em(face: &ttf_parser::Face<'_>, glyph: ttf_parser::GlyphId) -> Option<f64> {
    let advance = face.glyph_hor_advance(glyph)?;
    Some(f64::from(glyph_width_1000(advance, face.units_per_em())) / 1000.0)
}

fn build_embedded_font(
    used: &BTreeSet<char>,
    source_font: &OutputFont,
    key: OutputFontKey,
) -> std::result::Result<(EmbeddedFont, BTreeMap<char, u16>), ()> {
    let bytes = &source_font.bytes;
    let face = configured_output_font_face(bytes, key)?;
    let instance = output_font_instance(bytes, key)?;
    let mut remapper = subsetter::GlyphRemapper::new();
    let mut original = Vec::new();
    for character in used {
        let glyph = face.glyph_index(*character).ok_or(())?;
        remapper.remap(glyph.0);
        original.push((*character, glyph));
    }
    let font_bytes = if instance.variations.is_empty() {
        subsetter::subset(bytes, 0, &remapper).map_err(|_| ())?
    } else {
        let variations = instance
            .variations
            .iter()
            .map(|variation| (subsetter::Tag::new(&variation.tag), variation.value))
            .collect::<Vec<_>>();
        subsetter::subset_with_variations(bytes, 0, &variations, &remapper).map_err(|_| ())?
    };
    let mut cids = BTreeMap::new();
    let mut glyphs = original
        .into_iter()
        .map(|(character, glyph)| {
            let cid = remapper.get(glyph.0).ok_or(())?;
            let advance = face.glyph_hor_advance(glyph).ok_or(())?;
            cids.insert(character, cid);
            Ok((cid, character, advance))
        })
        .collect::<std::result::Result<Vec<_>, ()>>()?;
    glyphs.sort_by_key(|value| value.0);
    let tag = subset_tag(used, key);
    let postscript_name = instance
        .postscript_name
        .unwrap_or_else(|| fallback_output_font_name(bytes, &source_font.postscript_name, key));
    Ok((
        EmbeddedFont {
            resource_name: key.resource_name().to_owned(),
            base_font: format!("{tag}+{postscript_name}"),
            font_bytes,
            units_per_em: face.units_per_em(),
            ascent: face.ascender(),
            descent: face.descender(),
            cap_height: face.capital_height().unwrap_or(face.ascender()),
            glyphs,
        },
        cids,
    ))
}

fn build_typeset_fonts<'a>(
    plans: impl IntoIterator<Item = &'a TypesetPlan>,
    configured_fonts: &OutputFonts,
) -> Result<BTreeMap<OutputFontKey, BuiltOutputFont>> {
    let plans = plans.into_iter().collect::<Vec<_>>();
    let faces = OutputFontFaces::parse(configured_fonts).map_err(|_| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "validated output font could not be parsed for translated text",
        )
    })?;
    let mut output_fonts = BTreeMap::new();
    for key in OutputFontKey::ALL {
        let used = plans
            .iter()
            .flat_map(|plan| plan.lines.iter().flatten())
            .filter(|character| faces.key_for(character.value, character.bold) == Some(key))
            .map(|character| character.value)
            .collect::<BTreeSet<_>>();
        if used.is_empty() {
            continue;
        }
        let source_font = match key {
            OutputFontKey::PrimaryRegular => &configured_fonts.regular,
            OutputFontKey::PrimaryBold => &configured_fonts.bold,
            OutputFontKey::FallbackRegular => &configured_fonts.fallback_regular,
            OutputFontKey::FallbackBold => &configured_fonts.fallback_bold,
        };
        let (font, cids) = build_embedded_font(&used, source_font, key).map_err(|_| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                "validated output font could not be subset for translated text",
            )
        })?;
        output_fonts.insert(key, BuiltOutputFont { font, cids });
    }
    Ok(output_fonts)
}

fn incompatible_final_ink_indices(
    planned_paragraphs: &[(usize, Vec<TypesetPlan>)],
    page: &il::Page,
    extracted: &ExtractedPage,
    crop_box: Rect,
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
) -> Result<BTreeSet<usize>> {
    let mut publications = Vec::<(usize, il::PublicationInk)>::new();
    let mut incompatible = BTreeSet::new();
    for (index, (reading_order, plans)) in planned_paragraphs.iter().enumerate() {
        let paragraph = page
            .paragraphs
            .iter()
            .find(|paragraph| paragraph.reading_order == *reading_order)
            .expect("planned paragraph belongs to its page");
        if paragraph.translated_text.as_deref() == Some(paragraph.source_text().as_str()) {
            continue;
        }
        let publication = planned_publication_ink(page.index, crop_box, paragraph, plans, fonts)?;
        if publication_has_retained_ink_collision(&publication, plans, extracted) {
            incompatible.insert(index);
        }
        publications.push((index, publication));
    }
    for (left_offset, (_, left)) in publications.iter().enumerate() {
        for (right_index, right) in &publications[left_offset + 1..] {
            if publications_overlap(left, right) {
                incompatible.insert(*right_index);
            }
        }
    }
    Ok(expand_incompatible_plan_indices(
        planned_paragraphs,
        incompatible,
    ))
}

fn expand_incompatible_plan_indices(
    planned_paragraphs: &[(usize, Vec<TypesetPlan>)],
    mut incompatible: BTreeSet<usize>,
) -> BTreeSet<usize> {
    let paragraph_spans = planned_paragraphs
        .iter()
        .map(|(_, plans)| {
            plans
                .iter()
                .flat_map(plan_modified_spans)
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    let mut blocked_spans = incompatible
        .iter()
        .flat_map(|index| paragraph_spans[*index].iter().copied())
        .collect::<BTreeSet<_>>();
    loop {
        let joined = paragraph_spans
            .iter()
            .enumerate()
            .filter(|(index, spans)| {
                !incompatible.contains(index) && !spans.is_disjoint(&blocked_spans)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if joined.is_empty() {
            return incompatible;
        }
        for index in joined {
            blocked_spans.extend(paragraph_spans[index].iter().copied());
            incompatible.insert(index);
        }
    }
}

fn publication_has_retained_ink_collision(
    publication: &il::PublicationInk,
    plans: &[TypesetPlan],
    extracted: &ExtractedPage,
) -> bool {
    let claimed_path_scopes =
        plans
            .iter()
            .flat_map(|plan| {
                plan.text_vector_relocations
                    .iter()
                    .map(|relocation| relocation.replay.span)
                    .chain(plan.formula_relocations.iter().flat_map(|relocation| {
                        relocation.vector_paths.iter().map(|path| path.span)
                    }))
            })
            .map(|span| (span.0, span.1))
            .collect::<BTreeSet<_>>();
    let claimed_images = plans
        .iter()
        .flat_map(|plan| &plan.formula_relocations)
        .flat_map(|relocation| relocation.inline_images.iter().map(|image| image.span))
        .collect::<BTreeSet<_>>();
    publication_glyph_bounds(publication).any(|glyph_bounds| {
        extracted.path_ink.iter().any(|ink| {
            !ink.replay_scope
                .is_some_and(|scope| claimed_path_scopes.contains(&scope))
                && !(ink.filled
                    && retained_fill_contains_rect(ink, publication.admissible_container))
                && retained_path_intersects_rect(ink, glyph_bounds)
        }) || extracted.inline_images.iter().any(|image| {
            let span = (image.content_object, image.byte_start, image.byte_end);
            !claimed_images.contains(&span)
                && !walked_image_contains_rect(image, publication.admissible_container)
                && walked_image_intersects_rect(image, glyph_bounds)
        })
    })
}

fn publications_overlap(left: &il::PublicationInk, right: &il::PublicationInk) -> bool {
    let right_bounds = publication_collision_bounds(right).collect::<Vec<_>>();
    publication_collision_bounds(left).any(|left_bound| {
        right_bounds
            .iter()
            .any(|&right_bound| rects_overlap(left_bound, right_bound, 0.01))
    })
}

fn publication_glyph_bounds(publication: &il::PublicationInk) -> impl Iterator<Item = Rect> + '_ {
    publication
        .components
        .iter()
        .flat_map(|component| match component {
            il::PublicationInkComponent::TranslatedText { glyphs, .. }
            | il::PublicationInkComponent::SourceTextReplay { glyphs, .. } => glyphs
                .iter()
                .map(|glyph| glyph.ink_bounds)
                .filter(|bounds| bounds.right > bounds.left && bounds.top > bounds.bottom)
                .collect::<Vec<_>>(),
            il::PublicationInkComponent::VectorPath { .. }
            | il::PublicationInkComponent::InlineImage { .. } => Vec::new(),
        })
}

fn publication_collision_bounds(
    publication: &il::PublicationInk,
) -> impl Iterator<Item = Rect> + '_ {
    publication
        .components
        .iter()
        .flat_map(|component| match component {
            il::PublicationInkComponent::TranslatedText { glyphs, .. }
            | il::PublicationInkComponent::SourceTextReplay { glyphs, .. } => glyphs
                .iter()
                .map(|glyph| glyph.ink_bounds)
                .filter(|bounds| bounds.right > bounds.left && bounds.top > bounds.bottom)
                .collect::<Vec<_>>(),
            il::PublicationInkComponent::VectorPath { bounds, .. }
            | il::PublicationInkComponent::InlineImage { bounds, .. } => vec![*bounds],
        })
}

fn retained_path_intersects_rect(ink: &crate::walk::WalkedPathInk, rect: Rect) -> bool {
    if intersection_area(ink.bounds, rect) <= 0.0001
        || ink
            .form_clip
            .is_some_and(|clip| intersection_area(clip, rect) <= 0.0001)
        || ink
            .clips
            .iter()
            .any(|clip| !walked_clip_intersects_rect(clip, rect))
    {
        return false;
    }
    if ink.filled {
        return ink
            .segments
            .iter()
            .any(|segment| path_segment_intersects_rect(*segment, rect))
            || rect_corners(rect)
                .into_iter()
                .any(|point| point_in_retained_fill(point, ink));
    }
    let expanded = expanded_rect(rect, ink.stroke_radius);
    ink.segments
        .iter()
        .any(|segment| path_segment_intersects_rect(*segment, expanded))
}

fn retained_fill_contains_rect(ink: &crate::walk::WalkedPathInk, rect: Rect) -> bool {
    if !ink.filled
        || ink
            .form_clip
            .is_some_and(|clip| !rect_contains(clip, rect, FINAL_INK_GEOMETRY_TOLERANCE_PT))
        || ink
            .clips
            .iter()
            .any(|clip| !walked_clip_contains_rect(clip, rect))
    {
        return false;
    }
    rect_corners(rect)
        .into_iter()
        .chain([Point {
            x: (rect.left + rect.right) / 2.0,
            y: (rect.bottom + rect.top) / 2.0,
        }])
        .all(|point| point_in_retained_fill(point, ink))
        && !ink
            .segments
            .iter()
            .any(|segment| path_segment_intersects_rect(*segment, rect))
}

fn walked_image_intersects_rect(image: &crate::walk::WalkedInlineImage, rect: Rect) -> bool {
    walked_image_bounds(image).is_some_and(|bounds| intersection_area(bounds, rect) > 0.0001)
        && image
            .clips
            .iter()
            .all(|clip| walked_clip_intersects_rect(clip, rect))
}

fn walked_image_contains_rect(image: &crate::walk::WalkedInlineImage, rect: Rect) -> bool {
    rect_contains(image.bounds, rect, FINAL_INK_GEOMETRY_TOLERANCE_PT)
        && image
            .form_clip
            .is_none_or(|clip| rect_contains(clip, rect, FINAL_INK_GEOMETRY_TOLERANCE_PT))
        && image
            .clips
            .iter()
            .all(|clip| walked_clip_contains_rect(clip, rect))
}

fn walked_clip_intersects_rect(clip: &crate::walk::WalkedClipPath, rect: Rect) -> bool {
    let Some(bounds) = clip.bounds else {
        return false;
    };
    if intersection_area(bounds, rect) <= 0.0001 {
        return false;
    }
    clip.segments
        .iter()
        .any(|segment| path_segment_intersects_rect(*segment, rect))
        || rect_corners(rect)
            .into_iter()
            .any(|point| point_in_walked_clip(point, clip))
        || clip
            .segments
            .iter()
            .any(|segment| point_in_rect(segment.start, rect))
}

fn walked_clip_contains_rect(clip: &crate::walk::WalkedClipPath, rect: Rect) -> bool {
    clip.bounds
        .is_some_and(|bounds| rect_contains(bounds, rect, FINAL_INK_GEOMETRY_TOLERANCE_PT))
        && rect_corners(rect)
            .into_iter()
            .chain([Point {
                x: (rect.left + rect.right) / 2.0,
                y: (rect.bottom + rect.top) / 2.0,
            }])
            .all(|point| point_in_walked_clip(point, clip))
        && !clip
            .segments
            .iter()
            .any(|segment| path_segment_intersects_rect(*segment, rect))
}

fn point_in_walked_clip(point: Point, clip: &crate::walk::WalkedClipPath) -> bool {
    if clip.even_odd {
        return clip.segments.iter().fold(false, |inside, segment| {
            let crosses = (segment.start.y > point.y) != (segment.end.y > point.y)
                && point.x
                    < (segment.end.x - segment.start.x) * (point.y - segment.start.y)
                        / (segment.end.y - segment.start.y)
                        + segment.start.x;
            inside ^ crosses
        });
    }
    clip.segments.iter().fold(0_i32, |winding, segment| {
        let cross = (segment.end.x - segment.start.x) * (point.y - segment.start.y)
            - (point.x - segment.start.x) * (segment.end.y - segment.start.y);
        if segment.start.y <= point.y && segment.end.y > point.y && cross > 0.0 {
            winding + 1
        } else if segment.start.y > point.y && segment.end.y <= point.y && cross < 0.0 {
            winding - 1
        } else {
            winding
        }
    }) != 0
}

fn point_in_retained_fill(point: Point, ink: &crate::walk::WalkedPathInk) -> bool {
    if ink.even_odd {
        return ink.segments.iter().fold(false, |inside, segment| {
            let crosses = (segment.start.y > point.y) != (segment.end.y > point.y)
                && point.x
                    < (segment.end.x - segment.start.x) * (point.y - segment.start.y)
                        / (segment.end.y - segment.start.y)
                        + segment.start.x;
            inside ^ crosses
        });
    }
    ink.segments.iter().fold(0_i32, |winding, segment| {
        let cross = (segment.end.x - segment.start.x) * (point.y - segment.start.y)
            - (point.x - segment.start.x) * (segment.end.y - segment.start.y);
        if segment.start.y <= point.y && segment.end.y > point.y && cross > 0.0 {
            winding + 1
        } else if segment.start.y > point.y && segment.end.y <= point.y && cross < 0.0 {
            winding - 1
        } else {
            winding
        }
    }) != 0
}

fn path_segment_intersects_rect(segment: crate::walk::WalkedPathSegment, rect: Rect) -> bool {
    let dx = segment.end.x - segment.start.x;
    let dy = segment.end.y - segment.start.y;
    let mut minimum = 0.0_f64;
    let mut maximum = 1.0_f64;
    for (p, q) in [
        (-dx, segment.start.x - rect.left),
        (dx, rect.right - segment.start.x),
        (-dy, segment.start.y - rect.bottom),
        (dy, rect.top - segment.start.y),
    ] {
        if p.abs() <= 1e-12 {
            if q < 0.0 {
                return false;
            }
            continue;
        }
        let ratio = q / p;
        if p < 0.0 {
            minimum = minimum.max(ratio);
        } else {
            maximum = maximum.min(ratio);
        }
        if minimum > maximum {
            return false;
        }
    }
    true
}

fn rect_corners(rect: Rect) -> [Point; 4] {
    [
        Point {
            x: rect.left,
            y: rect.bottom,
        },
        Point {
            x: rect.left,
            y: rect.top,
        },
        Point {
            x: rect.right,
            y: rect.bottom,
        },
        Point {
            x: rect.right,
            y: rect.top,
        },
    ]
}

fn expanded_rect(rect: Rect, amount: f64) -> Rect {
    Rect {
        left: rect.left - amount,
        bottom: rect.bottom - amount,
        right: rect.right + amount,
        top: rect.top + amount,
    }
}

fn rects_overlap(left: Rect, right: Rect, tolerance: f64) -> bool {
    left.right > right.left + tolerance
        && right.right > left.left + tolerance
        && left.top > right.bottom + tolerance
        && right.top > left.bottom + tolerance
}

#[allow(clippy::too_many_arguments)]
fn incompatible_plan_component_indices(
    planned_paragraphs: &[(usize, Vec<TypesetPlan>)],
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
    streams: &BTreeMap<lopdf::ObjectId, &[u8]>,
    content_transforms: &BTreeMap<SpanKey, [f64; 6]>,
    text_show_states: &BTreeMap<SpanKey, TextShowState>,
    existing_replacements: &BTreeMap<SpanKey, Vec<u8>>,
) -> Result<BTreeSet<usize>> {
    let paragraph_spans = planned_paragraphs
        .iter()
        .map(|(_, plans)| {
            plans
                .iter()
                .flat_map(plan_modified_spans)
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    let mut remaining = (0..planned_paragraphs.len()).collect::<BTreeSet<_>>();
    let mut incompatible = BTreeSet::new();
    while let Some(seed) = remaining.iter().next().copied() {
        remaining.remove(&seed);
        let mut component = vec![seed];
        let mut component_spans = paragraph_spans[seed].clone();
        loop {
            let joined = remaining
                .iter()
                .copied()
                .filter(|index| !paragraph_spans[*index].is_disjoint(&component_spans))
                .collect::<Vec<_>>();
            if joined.is_empty() {
                break;
            }
            for index in joined {
                remaining.remove(&index);
                component_spans.extend(paragraph_spans[index].iter().copied());
                component.push(index);
            }
        }

        let mut probe = existing_replacements.clone();
        'install: for index in &component {
            for plan in &planned_paragraphs[*index].1 {
                if let Err(error) = install_typeset_replacements(
                    plan,
                    fonts,
                    streams,
                    content_transforms,
                    text_show_states,
                    &mut probe,
                ) {
                    if is_typeset_replacement_collision(&error) {
                        incompatible.extend(component.iter().copied());
                        break 'install;
                    }
                    return Err(error);
                }
            }
        }
    }
    Ok(incompatible)
}

fn is_typeset_replacement_collision(error: &MimusError) -> bool {
    let MimusError::Internal {
        reason: InternalReason::InvariantViolation,
        message,
        ..
    } = error
    else {
        return false;
    };
    matches!(
        message.as_str(),
        "typeset span has an incompatible existing replacement"
            | "formula relocation overlaps another typeset replacement"
            | "vector relocation overlaps another typeset replacement"
            | "split formula span was not neutralized by its translated owner"
            | "split formula span has an incompatible translated replacement"
    )
}

fn subset_tag(used: &BTreeSet<char>, key: OutputFontKey) -> String {
    let mut hash = match key {
        OutputFontKey::PrimaryRegular => 0x811c9dc5u32,
        OutputFontKey::PrimaryBold => 0x811c9dc4u32,
        OutputFontKey::FallbackRegular => 0x811c9dc3u32,
        OutputFontKey::FallbackBold => 0x811c9dc2u32,
    };
    for character in used {
        hash ^= u32::from(*character);
        hash = hash.wrapping_mul(16_777_619);
    }
    (0..6)
        .map(|shift| char::from(b'A' + ((hash >> (shift * 5)) % 26) as u8))
        .collect()
}

fn install_typeset_replacements(
    plan: &TypesetPlan,
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
    streams: &BTreeMap<lopdf::ObjectId, &[u8]>,
    content_transforms: &BTreeMap<SpanKey, [f64; 6]>,
    text_show_states: &BTreeMap<SpanKey, TextShowState>,
    replacements: &mut BTreeMap<SpanKey, Vec<u8>>,
) -> Result<()> {
    install_text_replacements(
        plan,
        fonts,
        streams,
        content_transforms,
        text_show_states,
        replacements,
    )?;
    for relocation in &plan.text_vector_relocations {
        install_vector_relocation(
            relocation.replay,
            relocation.delta_x_pt,
            relocation.delta_y_pt,
            streams,
            replacements,
        )?;
    }
    for relocation in &plan.formula_relocations {
        install_formula_relocation(
            relocation,
            streams,
            content_transforms,
            text_show_states,
            replacements,
        )?;
    }
    Ok(())
}

fn install_vector_relocation(
    replay: FormulaVectorReplay,
    delta_x_pt: f64,
    delta_y_pt: f64,
    streams: &BTreeMap<lopdf::ObjectId, &[u8]>,
    replacements: &mut BTreeMap<SpanKey, Vec<u8>>,
) -> Result<()> {
    let source = streams
        .get(&replay.span.0)
        .and_then(|stream| stream.get(replay.span.1..replay.span.2))
        .ok_or_else(|| span_out_of_bounds(replay.span.0, replay.span.1, replay.span.2, 0))?;
    let (delta_x, delta_y) =
        content_relative_delta(replay.content_transform, delta_x_pt, delta_y_pt).ok_or_else(
            || {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "vector path has a singular content transform",
                )
            },
        )?;
    let mut replacement = format!(
        "q\n1 0 0 1 {} {} cm\n",
        pdf_number(delta_x),
        pdf_number(delta_y)
    )
    .into_bytes();
    replacement.extend_from_slice(source);
    replacement.extend_from_slice(b"\nQ\n");
    if replacements.insert(replay.span, replacement).is_some() {
        return Err(MimusError::internal(
            InternalReason::InvariantViolation,
            "vector relocation overlaps another typeset replacement",
        ));
    }
    Ok(())
}

fn install_text_replacements(
    plan: &TypesetPlan,
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
    streams: &BTreeMap<lopdf::ObjectId, &[u8]>,
    content_transforms: &BTreeMap<SpanKey, [f64; 6]>,
    text_show_states: &BTreeMap<SpanKey, TextShowState>,
    replacements: &mut BTreeMap<SpanKey, Vec<u8>>,
) -> Result<()> {
    let empty_operand = |span: SpanKey| -> Result<Vec<u8>> {
        let source = streams
            .get(&span.0)
            .and_then(|stream| stream.get(span.1..span.2))
            .ok_or_else(|| span_out_of_bounds(span.0, span.1, span.2, 0))?;
        Ok(if source.starts_with(b"[") && source.ends_with(b"]") {
            b"[]".to_vec()
        } else {
            b"()".to_vec()
        })
    };
    if plan.lines.is_empty() {
        for span in &plan.spans {
            if let std::collections::btree_map::Entry::Vacant(entry) = replacements.entry(*span) {
                entry.insert(state_preserving_empty_replacement(
                    *span,
                    streams,
                    text_show_states,
                )?);
            }
        }
        return Ok(());
    }
    let content_transform = content_transforms.get(&plan.spans[0]).ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "typeset span has no active content transform",
        )
    })?;
    // The replacement is emitted inside q/Q but inherits the source text state. Typeset geometry
    // assumes neutral spacing, horizontal scale, rise, and rendering mode, so make that state
    // explicit before placing any output glyphs.
    let mut command = String::from("0 Tc 0 Tw 100 Tz 0 Ts 0 Tr\n");
    let mut emitted_run = false;
    for (index, line) in plan.lines.iter().enumerate() {
        let (x, y) = plan.baselines[index];
        let matrix = content_relative_text_matrix(*content_transform, x, y).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                "typeset span has a singular content transform",
            )
        })?;
        if emitted_run {
            command.push_str(" Tj\n");
        }
        command.push_str(&format!(
            "{} {} {} {} {} {} Tm ",
            pdf_number(matrix[0]),
            pdf_number(matrix[1]),
            pdf_number(matrix[2]),
            pdf_number(matrix[3]),
            pdf_number(matrix[4]),
            pdf_number(matrix[5])
        ));
        let mut run_start = 0;
        while run_start < line.len() {
            let key = built_font_key(fonts, line[run_start]).ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "typeset character has no embedded font",
                )
            })?;
            let mut run_end = run_start + 1;
            while run_end < line.len() && built_font_key(fonts, line[run_end]) == Some(key) {
                run_end += 1;
            }
            if index == 0
                && let Some(gap) = plan.section_number_gap
                && gap.prefix_in_output
                && run_start < gap.prefix_glyph_count
                && run_end > gap.prefix_glyph_count
            {
                run_end = gap.prefix_glyph_count;
            }
            if emitted_run && run_start > 0 {
                if index == 0
                    && plan.section_number_gap.is_some_and(|gap| {
                        gap.prefix_in_output && run_start == gap.prefix_glyph_count
                    })
                {
                    let gap = plan.section_number_gap.unwrap();
                    let title_matrix =
                        content_relative_text_matrix(*content_transform, gap.output_title_left, y)
                            .ok_or_else(|| {
                                MimusError::internal(
                                    InternalReason::InvariantViolation,
                                    "section-number gap has a singular content transform",
                                )
                            })?;
                    command.push_str(&format!(
                        " Tj\n{} {} {} {} {} {} Tm ",
                        pdf_number(title_matrix[0]),
                        pdf_number(title_matrix[1]),
                        pdf_number(title_matrix[2]),
                        pdf_number(title_matrix[3]),
                        pdf_number(title_matrix[4]),
                        pdf_number(title_matrix[5])
                    ));
                } else {
                    command.push_str(" Tj ");
                }
            }
            let output_font = fonts.get(&key).ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "typeset style has no embedded font",
                )
            })?;
            command.push_str(&format!(
                "/{} {} Tf <",
                output_font.font.resource_name,
                pdf_number(plan.font_size)
            ));
            for character in &line[run_start..run_end] {
                let cid = output_font.cids.get(&character.value).ok_or_else(|| {
                    MimusError::internal(
                        InternalReason::InvariantViolation,
                        "typeset glyph has no output CID",
                    )
                })?;
                command.push_str(&format!("{cid:04X}"));
            }
            command.push('>');
            emitted_run = true;
            run_start = run_end;
        }
    }
    let first = plan.spans[0];
    let first_source = streams
        .get(&first.0)
        .and_then(|stream| stream.get(first.1..first.2))
        .ok_or_else(|| span_out_of_bounds(first.0, first.1, first.2, 0))?;
    let terminal = empty_operand(first)?;
    let original_operator = if first_source.starts_with(b"[") && first_source.ends_with(b"]") {
        b" TJ\n".as_slice()
    } else {
        b" Tj\n".as_slice()
    };
    let state_tail = text_show_state_tail(first, streams, text_show_states)?;
    let mut graphics_tail = b"Q\n".to_vec();
    graphics_tail.extend_from_slice(&state_tail);
    let neutralized = state_preserving_empty_replacement(first, streams, text_show_states)?;
    let replacement = replacements.entry(first).or_default();
    if replacement.is_empty()
        || replacement.as_slice() == terminal.as_slice()
        || replacement.as_slice() == neutralized.as_slice()
    {
        replacement.clear();
        replacement.extend_from_slice(&terminal);
        replacement.extend_from_slice(original_operator);
        replacement.extend_from_slice(b"q\n");
    } else if replacement.ends_with(&graphics_tail) {
        replacement.truncate(replacement.len() - graphics_tail.len());
    } else {
        return Err(MimusError::internal(
            InternalReason::InvariantViolation,
            "typeset span has an incompatible existing replacement",
        ));
    }
    replacement.extend_from_slice(command.as_bytes());
    replacement.extend_from_slice(b" Tj\n");
    replacement.extend_from_slice(&graphics_tail);
    for span in &plan.spans[1..] {
        if let std::collections::btree_map::Entry::Vacant(entry) = replacements.entry(*span) {
            entry.insert(state_preserving_empty_replacement(
                *span,
                streams,
                text_show_states,
            )?);
        }
    }
    Ok(())
}

fn install_formula_relocation(
    relocation: &FormulaRelocation,
    streams: &BTreeMap<lopdf::ObjectId, &[u8]>,
    content_transforms: &BTreeMap<SpanKey, [f64; 6]>,
    text_show_states: &BTreeMap<SpanKey, TextShowState>,
    replacements: &mut BTreeMap<SpanKey, Vec<u8>>,
) -> Result<()> {
    for span in &relocation.spans {
        if let Some(glyphs) = relocation.split_glyphs.get(span) {
            install_split_formula_relocation(
                *span,
                glyphs,
                relocation.delta_x_pt,
                relocation.delta_y_pt,
                streams,
                content_transforms,
                text_show_states,
                replacements,
            )?;
            continue;
        }
        let source = streams
            .get(&span.0)
            .and_then(|stream| stream.get(span.1..span.2))
            .ok_or_else(|| span_out_of_bounds(span.0, span.1, span.2, 0))?;
        let (empty, operator) = if source.starts_with(b"[") && source.ends_with(b"]") {
            (b"[]".as_slice(), b" TJ\n".as_slice())
        } else {
            (b"()".as_slice(), b" Tj\n".as_slice())
        };
        let content_transform = content_transforms.get(span).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                "formula span has no active content transform",
            )
        })?;
        let (delta_x, delta_y) = content_relative_delta(
            *content_transform,
            relocation.delta_x_pt,
            relocation.delta_y_pt,
        )
        .ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                "formula span has a singular content transform",
            )
        })?;
        let mut replacement = empty.to_vec();
        replacement.extend_from_slice(operator);
        replacement.extend_from_slice(
            format!(
                "q\n1 0 0 1 {} {} cm\n",
                pdf_number(delta_x),
                pdf_number(delta_y)
            )
            .as_bytes(),
        );
        replacement.extend_from_slice(source);
        replacement.extend_from_slice(operator);
        replacement.extend_from_slice(b"Q\n");
        replacement.extend_from_slice(&text_show_state_tail(*span, streams, text_show_states)?);
        if replacements.insert(*span, replacement).is_some() {
            return Err(MimusError::internal(
                InternalReason::InvariantViolation,
                "formula relocation overlaps another typeset replacement",
            ));
        }
    }
    for path in relocation
        .vector_paths
        .iter()
        .chain(&relocation.inline_images)
    {
        install_vector_relocation(
            *path,
            relocation.delta_x_pt,
            relocation.delta_y_pt,
            streams,
            replacements,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_split_formula_relocation(
    span: SpanKey,
    glyphs: &[FormulaGlyphReplay],
    delta_x_pt: f64,
    delta_y_pt: f64,
    streams: &BTreeMap<lopdf::ObjectId, &[u8]>,
    content_transforms: &BTreeMap<SpanKey, [f64; 6]>,
    text_show_states: &BTreeMap<SpanKey, TextShowState>,
    replacements: &mut BTreeMap<SpanKey, Vec<u8>>,
) -> Result<()> {
    let content_transform = content_transforms.get(&span).ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "split formula span has no active content transform",
        )
    })?;
    let (delta_x, delta_y) = content_relative_delta(*content_transform, delta_x_pt, delta_y_pt)
        .ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                "split formula span has a singular content transform",
            )
        })?;
    let state_tail = text_show_state_tail(span, streams, text_show_states)?;
    let graphics_tail = [b"Q\n".as_slice(), state_tail.as_slice()].concat();
    let replacement = replacements.get_mut(&span).ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "split formula span was not neutralized by its translated owner",
        )
    })?;
    if replacement.ends_with(&graphics_tail) {
        replacement.truncate(replacement.len() - graphics_tail.len());
    } else if replacement.ends_with(&state_tail) {
        replacement.truncate(replacement.len() - state_tail.len());
        replacement.extend_from_slice(b"q\n");
    } else {
        return Err(MimusError::internal(
            InternalReason::InvariantViolation,
            "split formula span has an incompatible translated replacement",
        ));
    }
    replacement.extend_from_slice(
        format!(
            "q\n1 0 0 1 {} {} cm\n",
            pdf_number(delta_x),
            pdf_number(delta_y)
        )
        .as_bytes(),
    );
    for glyph in glyphs {
        let [a, b, c, d, e, f] = glyph.text_matrix;
        replacement.extend_from_slice(
            format!(
                "/{} {} Tf\n{} {} {} {} {} {} Tm <",
                glyph.font_resource_name,
                pdf_number(glyph.font_size),
                pdf_number(a),
                pdf_number(b),
                pdf_number(c),
                pdf_number(d),
                pdf_number(e),
                pdf_number(f)
            )
            .as_bytes(),
        );
        for byte in &glyph.encoded {
            replacement.extend_from_slice(format!("{byte:02X}").as_bytes());
        }
        replacement.extend_from_slice(b"> Tj\n");
    }
    replacement.extend_from_slice(b"Q\n");
    replacement.extend_from_slice(&graphics_tail);
    Ok(())
}

fn built_font_key(
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
    character: crate::translate::StyledCharacter,
) -> Option<OutputFontKey> {
    let primary = OutputFontKey::for_style(character.bold, false);
    if fonts
        .get(&primary)
        .is_some_and(|font| font.cids.contains_key(&character.value))
    {
        return Some(primary);
    }
    let fallback = OutputFontKey::for_style(character.bold, true);
    fonts
        .get(&fallback)
        .is_some_and(|font| font.cids.contains_key(&character.value))
        .then_some(fallback)
}

fn state_preserving_empty_replacement(
    span: SpanKey,
    streams: &BTreeMap<lopdf::ObjectId, &[u8]>,
    text_show_states: &BTreeMap<SpanKey, TextShowState>,
) -> Result<Vec<u8>> {
    let source = streams
        .get(&span.0)
        .and_then(|stream| stream.get(span.1..span.2))
        .ok_or_else(|| span_out_of_bounds(span.0, span.1, span.2, 0))?;
    let empty = if source.starts_with(b"[") && source.ends_with(b"]") {
        b"[]".as_slice()
    } else {
        b"()".as_slice()
    };
    let operator = if source.starts_with(b"[") && source.ends_with(b"]") {
        b" TJ\n".as_slice()
    } else {
        b" Tj\n".as_slice()
    };
    let mut replacement = empty.to_vec();
    replacement.extend_from_slice(operator);
    replacement.extend_from_slice(&text_show_state_tail(span, streams, text_show_states)?);
    Ok(replacement)
}

fn text_show_state_tail(
    span: SpanKey,
    streams: &BTreeMap<lopdf::ObjectId, &[u8]>,
    text_show_states: &BTreeMap<SpanKey, TextShowState>,
) -> Result<Vec<u8>> {
    let source = streams
        .get(&span.0)
        .and_then(|stream| stream.get(span.1..span.2))
        .ok_or_else(|| span_out_of_bounds(span.0, span.1, span.2, 0))?;
    let terminal = if source.starts_with(b"[") && source.ends_with(b"]") {
        b"[]".as_slice()
    } else {
        b"()".as_slice()
    };
    let state = text_show_states.get(&span).ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "typeset span has no source text-show state",
        )
    })?;
    let [a, b, c, d, e, f] = state.line_matrix;
    let [after_a, after_b, after_c, after_d, after_e, after_f] = state.matrix_after_show;
    let coefficient_error = (a - after_a)
        .abs()
        .max((b - after_b).abs())
        .max((c - after_c).abs())
        .max((d - after_d).abs());
    let denominator = a.mul_add(a, b * b);
    if coefficient_error > 1e-7 || denominator <= 1e-12 {
        return Err(MimusError::internal(
            InternalReason::InvariantViolation,
            "source text-show matrices cannot be restored",
        ));
    }
    let advance = ((after_e - e) * a + (after_f - f) * b) / denominator;
    let residual_x = after_e - a.mul_add(advance, e);
    let residual_y = after_f - b.mul_add(advance, f);
    if residual_x.abs().max(residual_y.abs()) > 1e-7 {
        return Err(MimusError::internal(
            InternalReason::InvariantViolation,
            "source text-show advance is not horizontal",
        ));
    }
    let scale = state.font_size * state.horizontal_scale;
    if advance.abs() > 1e-12 && scale.abs() <= 1e-12 {
        return Err(MimusError::internal(
            InternalReason::InvariantViolation,
            "source text-show advance has a zero text scale",
        ));
    }

    let mut tail = format!(
        "{} {} {} {} {} {} Tm\n",
        pdf_number(a),
        pdf_number(b),
        pdf_number(c),
        pdf_number(d),
        pdf_number(e),
        pdf_number(f)
    )
    .into_bytes();
    if advance.abs() > 1e-12 {
        let adjustment = -advance * 1000.0 / scale;
        tail.extend_from_slice(format!("[{}] TJ\n", pdf_number(adjustment)).as_bytes());
    }
    tail.extend_from_slice(terminal);
    Ok(tail)
}

fn content_relative_text_matrix(content: [f64; 6], x: f64, y: f64) -> Option<[f64; 6]> {
    let [a, b, c, d, e, f] = content;
    let determinant = a.mul_add(d, -(b * c));
    if !determinant.is_finite() || determinant.abs() <= 1e-12 {
        return None;
    }
    let inverse = [
        d / determinant,
        -b / determinant,
        -c / determinant,
        a / determinant,
        (c * f - d * e) / determinant,
        (b * e - a * f) / determinant,
    ];
    Some([
        inverse[0],
        inverse[1],
        inverse[2],
        inverse[3],
        inverse[0].mul_add(x, inverse[2].mul_add(y, inverse[4])),
        inverse[1].mul_add(x, inverse[3].mul_add(y, inverse[5])),
    ])
}

fn content_relative_delta(content: [f64; 6], x: f64, y: f64) -> Option<(f64, f64)> {
    let [a, b, c, d, _, _] = content;
    let determinant = a.mul_add(d, -(b * c));
    if !determinant.is_finite() || determinant.abs() <= 1e-12 {
        return None;
    }
    Some(((d * x - c * y) / determinant, (a * y - b * x) / determinant))
}

fn planned_characters(
    plan: &TypesetPlan,
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
) -> Vec<TypesetCharacter> {
    let mut output = planned_line_characters(plan, fonts)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    output.extend(
        plan.formula_relocations
            .iter()
            .flat_map(|relocation| relocation.characters.iter().cloned()),
    );
    output
}

fn planned_line_characters(
    plan: &TypesetPlan,
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
) -> Vec<Vec<TypesetCharacter>> {
    let mut output = Vec::with_capacity(plan.lines.len());
    for (line_index, (line, &(start_x, baseline_y))) in
        plan.lines.iter().zip(&plan.baselines).enumerate()
    {
        let mut x = start_x;
        let mut planned_line = Vec::with_capacity(line.len());
        for (character_index, character) in line.iter().enumerate() {
            if line_index == 0
                && plan.section_number_gap.is_some_and(|gap| {
                    gap.prefix_in_output && character_index == gap.prefix_glyph_count
                })
            {
                x += plan.section_number_gap.unwrap().gap_pt;
            }
            let key =
                built_font_key(fonts, *character).expect("typeset character has an embedded font");
            let font = &fonts[&key].font;
            let advance = font
                .glyphs
                .iter()
                .find_map(|(_, value, advance)| (*value == character.value).then_some(*advance))
                .expect("typeset glyph exists in its embedded font");
            planned_line.push(TypesetCharacter {
                unicode: character.value,
                baseline_origin: il::Point { x, y: baseline_y },
            });
            x += f64::from(glyph_width_1000(advance, font.units_per_em)) / 1000.0 * plan.font_size;
        }
        output.push(planned_line);
    }
    output
}

fn planned_line_publication_glyphs(
    plan: &TypesetPlan,
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
) -> Result<Vec<Vec<il::PublicationGlyph>>> {
    let faces = fonts
        .iter()
        .map(|(&key, font)| {
            ttf_parser::Face::parse(&font.font.font_bytes, 0)
                .map(|face| (key, face))
                .map_err(|_| {
                    MimusError::internal(
                        InternalReason::InvariantViolation,
                        "embedded output font could not be parsed for publication ink",
                    )
                })
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut output = Vec::with_capacity(plan.lines.len());
    for (line_index, (line, &(start_x, baseline_y))) in
        plan.lines.iter().zip(&plan.baselines).enumerate()
    {
        let mut x = start_x;
        let mut planned_line = Vec::with_capacity(line.len());
        for (character_index, character) in line.iter().enumerate() {
            if line_index == 0
                && plan.section_number_gap.is_some_and(|gap| {
                    gap.prefix_in_output && character_index == gap.prefix_glyph_count
                })
            {
                x += plan.section_number_gap.unwrap().gap_pt;
            }
            let key = built_font_key(fonts, *character).ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "typeset character has no embedded font for publication ink",
                )
            })?;
            let font = &fonts[&key];
            let cid = *font.cids.get(&character.value).ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "typeset glyph has no output CID for publication ink",
                )
            })?;
            let advance = font
                .font
                .glyphs
                .iter()
                .find_map(|(candidate, _, advance)| (*candidate == cid).then_some(*advance))
                .ok_or_else(|| {
                    MimusError::internal(
                        InternalReason::InvariantViolation,
                        "typeset glyph has no output advance for publication ink",
                    )
                })?;
            let face = &faces[&key];
            let scale = plan.font_size / f64::from(face.units_per_em());
            let baseline_origin = il::Point { x, y: baseline_y };
            let ink_bounds = face.glyph_bounding_box(ttf_parser::GlyphId(cid)).map_or(
                Rect {
                    left: x,
                    bottom: baseline_y,
                    right: x,
                    top: baseline_y,
                },
                |bounds| Rect {
                    left: x + f64::from(bounds.x_min) * scale,
                    bottom: baseline_y + f64::from(bounds.y_min) * scale,
                    right: x + f64::from(bounds.x_max) * scale,
                    top: baseline_y + f64::from(bounds.y_max) * scale,
                },
            );
            planned_line.push(il::PublicationGlyph {
                unicode: character.value,
                baseline_origin,
                ink_bounds,
            });
            x += f64::from(glyph_width_1000(advance, font.font.units_per_em)) / 1000.0
                * plan.font_size;
        }
        output.push(planned_line);
    }
    Ok(output)
}

fn planned_publication_ink(
    page_index: usize,
    crop_box: Rect,
    paragraph: &Paragraph,
    plans: &[TypesetPlan],
    fonts: &BTreeMap<OutputFontKey, BuiltOutputFont>,
) -> Result<il::PublicationInk> {
    let section_number_plans = plans
        .iter()
        .filter_map(|plan| plan.section_number_gap.map(|gap| (plan, gap)))
        .collect::<Vec<_>>();
    if section_number_plans.len() > 1 {
        return Err(MimusError::internal(
            InternalReason::InvariantViolation,
            "a publication has multiple retained section-number gaps",
        ));
    }
    let section_number_gap =
        section_number_plans
            .first()
            .map(|(plan, gap)| il::PublicationSectionNumberGap {
                prefix_glyph_count: gap.prefix_glyph_count,
                prefix_in_output: gap.prefix_in_output,
                source_prefix_left: gap.source_prefix_left,
                source_title_left: gap.source_title_left,
                output_prefix_width: gap.output_prefix_width,
                output_title_left: gap.output_title_left,
                gap_pt: gap.gap_pt,
                font_size: plan.font_size,
                clamped: gap.clamped,
            });
    let mut components = Vec::new();
    let mut next_formula_group = 1usize;
    for plan in plans {
        let line_characters = planned_line_publication_glyphs(plan, fonts)?;
        if line_characters.len() != plan.lines.len() || plan.ink_bounds.len() < plan.lines.len() {
            return Err(MimusError::internal(
                InternalReason::InvariantViolation,
                "final publication ink does not align with planned text lines",
            ));
        }
        for (line_index, glyphs) in line_characters.into_iter().enumerate() {
            if glyphs.is_empty() {
                continue;
            }
            components.push(il::PublicationInkComponent::TranslatedText {
                ownership_group: 0,
                bounds: plan.ink_bounds[line_index],
                glyphs,
            });
        }
        for relocation in &plan.text_vector_relocations {
            components.push(il::PublicationInkComponent::VectorPath {
                ownership_group: 0,
                bounds: translated_rect(
                    relocation.replay.bounds,
                    relocation.delta_x_pt,
                    relocation.delta_y_pt,
                ),
            });
        }
        for relocation in &plan.formula_relocations {
            if relocation.characters.len() != relocation.glyph_ink_bounds.len() {
                return Err(MimusError::internal(
                    InternalReason::InvariantViolation,
                    "formula publication glyphs do not align with their ink bounds",
                ));
            }
            let ownership_group = next_formula_group;
            next_formula_group += 1;
            components.push(il::PublicationInkComponent::SourceTextReplay {
                ownership_group,
                bounds: relocation.text_ink_bounds,
                glyphs: relocation
                    .characters
                    .iter()
                    .zip(&relocation.glyph_ink_bounds)
                    .map(|(glyph, &ink_bounds)| il::PublicationGlyph {
                        unicode: glyph.unicode,
                        baseline_origin: glyph.baseline_origin,
                        ink_bounds,
                    })
                    .collect(),
            });
            components.extend(relocation.vector_paths.iter().map(|path| {
                il::PublicationInkComponent::VectorPath {
                    ownership_group,
                    bounds: translated_rect(
                        path.bounds,
                        relocation.delta_x_pt,
                        relocation.delta_y_pt,
                    ),
                }
            }));
            components.extend(relocation.inline_images.iter().map(|image| {
                il::PublicationInkComponent::InlineImage {
                    ownership_group,
                    bounds: translated_rect(
                        image.bounds,
                        relocation.delta_x_pt,
                        relocation.delta_y_pt,
                    ),
                }
            }));
        }
    }
    let Some(component_bounds) = components
        .iter()
        .map(il::PublicationInkComponent::bounds)
        .reduce(Rect::union)
    else {
        return Err(MimusError::internal(
            InternalReason::InvariantViolation,
            "a non-identity publication has no final ink evidence",
        ));
    };
    let owning_container = paragraph_typeset_container(paragraph).ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "a non-identity publication has no owning layout container",
        )
    })?;
    let admissible_container = Rect {
        left: owning_container.left,
        bottom: owning_container.bottom.min(component_bounds.bottom),
        right: owning_container.right,
        top: owning_container.top.max(component_bounds.top),
    };
    Ok(il::PublicationInk {
        page_index,
        reading_order: paragraph.reading_order,
        crop_box,
        admissible_container,
        section_number_gap,
        components,
    })
}

fn pdf_number(value: f64) -> String {
    let mut output = format!("{value:.6}");
    while output.ends_with('0') {
        output.pop();
    }
    if output.ends_with('.') {
        output.pop();
    }
    output
}

pub fn write(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let pdf = document.pdf.as_ref().ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "Parse did not retain a PDF",
        )
    })?;
    let output_path = document.output_path().map(Path::to_owned).ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "Write received a document with no output path",
        )
    })?;
    let (candidate, report) = build_incremental_with_options(
        &document.original_bytes,
        pdf,
        &document.rewrites,
        WriteOptions {
            strip_link_borders: context.config.strip_link_borders,
            bilingual: context.config.bilingual,
        },
    )?;
    validate_output_roundtrip(document, context, &candidate)?;
    publish(&output_path, &candidate)?;
    if report.stripped_link_border_count > 0 {
        document.diagnostics.push(Diagnostic::LinkBordersStripped {
            annotation_count: report.stripped_link_border_count,
        });
    }
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
    let expected_page_count =
        document.extracted_pages.len() * if context.config.bilingual { 2 } else { 1 };
    if page_count != expected_page_count {
        return Err(output_mismatch(format!(
            "output has {page_count} pages; expected {expected_page_count}"
        )));
    }

    let rewritten_pages = document
        .rewrites
        .iter()
        .map(|rewrite| rewrite.page_index)
        .collect::<BTreeSet<_>>();
    for expected in &document.extracted_pages {
        if expected.degraded.is_some() && expected.frame.is_none() {
            continue;
        }
        let translated_page_index = if context.config.bilingual {
            expected.index * 2 + 1
        } else {
            expected.index
        };
        if context.config.bilingual {
            validate_bilingual_source_page(context, candidate, expected)?;
        }
        let geometry = context
            .engine
            .page_geometry(candidate, translated_page_index)
            .map_err(|error| {
                output_mismatch(format!(
                    "inspection engine rejected output page {} geometry: {error}",
                    translated_page_index + 1
                ))
            })?;
        validate_output_geometry(expected.index, expected.geometry, geometry)?;
        if !rewritten_pages.contains(&expected.index) {
            continue;
        }
        let characters = context
            .engine
            .page_characters(candidate, translated_page_index)
            .map_err(|error| {
                output_mismatch(format!(
                    "inspection engine rejected output page {} text: {error}",
                    translated_page_index + 1
                ))
            })?;
        let rewrite = document
            .rewrites
            .iter()
            .find(|rewrite| rewrite.page_index == expected.index)
            .ok_or_else(|| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "rewritten page has no rewrite plan",
                )
            })?;
        if rewrite.typeset_characters.is_empty() {
            validate_output_characters(
                expected.index,
                &expected.engine_characters,
                &characters,
                context.config.baseline_tolerance_pt,
            )?;
        } else {
            let retained =
                retained_input_characters(expected, rewrite, context.config.baseline_tolerance_pt)?;
            if retained.is_empty() {
                validate_typeset_characters(
                    expected.index,
                    &rewrite.typeset_characters,
                    &characters,
                    context.config.baseline_tolerance_pt,
                )?;
            } else {
                validate_mixed_output_characters(
                    expected.index,
                    &retained,
                    &rewrite.typeset_characters,
                    &characters,
                    context.config.baseline_tolerance_pt,
                )?;
            }
        }
        let raster = context
            .engine
            .rasterize_page_at_scale(
                candidate,
                translated_page_index,
                context.layout_detector.raster_pixels_per_point(),
            )
            .map_err(|error| {
                output_mismatch(format!(
                    "inspection engine rejected output page {} raster: {error}",
                    translated_page_index + 1
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
        if rewrite.typeset_characters.is_empty() {
            validate_output_raster(expected.index, input_raster, &raster)?;
        }
    }
    Ok(())
}

fn validate_bilingual_source_page(
    context: &PassContext<'_>,
    candidate: &[u8],
    expected: &ExtractedPage,
) -> Result<()> {
    let source_page_index = expected.index * 2;
    let geometry = context
        .engine
        .page_geometry(candidate, source_page_index)
        .map_err(|error| {
            output_mismatch(format!(
                "inspection engine rejected bilingual source page {} geometry: {error}",
                source_page_index + 1
            ))
        })?;
    validate_output_geometry(expected.index, expected.geometry, geometry)?;
    let characters = context
        .engine
        .page_characters(candidate, source_page_index)
        .map_err(|error| {
            output_mismatch(format!(
                "inspection engine rejected bilingual source page {} text: {error}",
                source_page_index + 1
            ))
        })?;
    validate_output_characters(
        expected.index,
        &expected.engine_characters,
        &characters,
        context.config.baseline_tolerance_pt,
    )?;
    if let Some(input_raster) = expected.input_raster.as_ref() {
        let raster = context
            .engine
            .rasterize_page_at_scale(
                candidate,
                source_page_index,
                context.layout_detector.raster_pixels_per_point(),
            )
            .map_err(|error| {
                output_mismatch(format!(
                    "inspection engine rejected bilingual source page {} raster: {error}",
                    source_page_index + 1
                ))
            })?;
        raster.validate()?;
        input_raster.validate()?;
        validate_output_raster(expected.index, input_raster, &raster)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ExpectedOutputCharacter {
    unicode: Option<char>,
    baseline_origin: il::Point,
}

fn retained_input_characters(
    page: &ExtractedPage,
    rewrite: &PageRewrite,
    baseline_tolerance_pt: f64,
) -> Result<Vec<ExpectedOutputCharacter>> {
    let streams = page
        .content_streams
        .iter()
        .map(|stream| (stream.object_id, stream.decoded.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let mut modified_spans = BTreeSet::new();
    for replacement in &rewrite.replacements {
        let original = streams
            .get(&replacement.content_object)
            .and_then(|stream| stream.get(replacement.byte_start..replacement.byte_end))
            .ok_or_else(|| {
                output_mismatch(format!(
                    "output page {} rewrite span {}:{}..{} is outside the input stream",
                    page.index + 1,
                    replacement.content_object.0,
                    replacement.byte_start,
                    replacement.byte_end
                ))
            })?;
        if original != replacement.replacement {
            modified_spans.insert((
                replacement.content_object.0,
                replacement.byte_start,
                replacement.byte_end,
            ));
        }
    }

    let modified_walk = page
        .walked_characters
        .iter()
        .map(|character| {
            modified_spans.contains(&(
                character.content_object.0,
                character.byte_start,
                character.byte_end,
            ))
        })
        .collect::<Vec<_>>();
    if !modified_walk.is_empty() && modified_walk.iter().all(|modified| *modified) {
        return Ok(Vec::new());
    }
    let mut engine_owners = vec![None; page.engine_characters.len()];
    for (walk_index, engine_index) in page
        .character_alignment
        .engine_indices_by_walk
        .iter()
        .enumerate()
        .filter_map(|(walk_index, engine_index)| engine_index.map(|value| (walk_index, value)))
    {
        if let Some(owner) = engine_owners.get_mut(engine_index) {
            *owner = Some(walk_index);
        }
    }
    if let Some(sequence) =
        sequence_engine_indices_by_walk(&page.walked_characters, &page.engine_characters)
    {
        for (walk_index, engine_index) in sequence.into_iter().enumerate() {
            let Some(engine_index) = engine_index else {
                continue;
            };
            if page.character_alignment.engine_indices_by_walk[walk_index].is_none()
                && engine_owners[engine_index].is_none()
            {
                engine_owners[engine_index] = Some(walk_index);
            }
        }
    }
    extend_engine_owners_by_contiguous_sequence(
        &page.walked_characters,
        &page.engine_characters,
        &mut engine_owners,
        baseline_tolerance_pt,
    );
    inherit_pdfium_ligature_expansion_owners(
        &page.walked_characters,
        &page.engine_characters,
        &mut engine_owners,
    );
    extend_engine_owners_by_contiguous_sequence(
        &page.walked_characters,
        &page.engine_characters,
        &mut engine_owners,
        baseline_tolerance_pt,
    );
    inherit_pdfium_utf16_surrogate_owners(
        &page.walked_characters,
        &page.engine_characters,
        &mut engine_owners,
    );
    inherit_unique_explanation_owners(
        &page.walked_characters,
        &page.engine_characters,
        &mut engine_owners,
        baseline_tolerance_pt,
    );
    let (owner_states, has_walk_candidates) = engine_modification_states(
        &page.walked_characters,
        &page.engine_characters,
        &engine_owners,
        &modified_walk,
        baseline_tolerance_pt,
    );
    let mut next_state = vec![None; owner_states.len()];
    let mut next = None;
    for index in (0..owner_states.len()).rev() {
        next_state[index] = next;
        if owner_states[index].is_some() {
            next = owner_states[index];
        }
    }
    let mut previous = None;
    let removed_engine_indices = owner_states
        .iter()
        .enumerate()
        .filter_map(|(index, state)| {
            if state.is_some() {
                previous = *state;
            }
            let removed = state.unwrap_or(false)
                || (state.is_none()
                    && !has_walk_candidates[index]
                    && previous == Some(true)
                    && next_state[index] == Some(true));
            removed.then_some(index)
        })
        .collect::<BTreeSet<_>>();

    Ok(page
        .engine_characters
        .iter()
        .enumerate()
        .filter(|(index, _)| !removed_engine_indices.contains(index))
        .map(|(_, character)| ExpectedOutputCharacter {
            unicode: character.unicode,
            baseline_origin: character.baseline_origin,
        })
        .collect())
}

fn engine_modification_states(
    walked: &[crate::walk::WalkedChar],
    engine: &[PageCharSnapshot],
    owners: &[Option<usize>],
    modified_walk: &[bool],
    tolerance: f64,
) -> (Vec<Option<bool>>, Vec<bool>) {
    let mut states = owners
        .iter()
        .map(|owner| owner.and_then(|walk_index| modified_walk.get(walk_index).copied()))
        .collect::<Vec<_>>();
    let mut has_walk_candidates = owners.iter().map(Option::is_some).collect::<Vec<_>>();
    if states.len() != engine.len()
        || walked.len() != modified_walk.len()
        || !tolerance.is_finite()
        || tolerance < 0.0
    {
        return (states, has_walk_candidates);
    }

    let cell_size = tolerance.max(f64::EPSILON);
    let mut buckets = HashMap::<(i64, i64), Vec<usize>>::new();
    for (walk_index, character) in walked.iter().enumerate() {
        if character.locatable
            && character.baseline_origin.x.is_finite()
            && character.baseline_origin.y.is_finite()
        {
            buckets
                .entry(baseline_grid_key(character.baseline_origin, cell_size))
                .or_default()
                .push(walk_index);
        }
    }

    for (engine_index, character) in engine.iter().enumerate() {
        if states[engine_index].is_some() || !valid_engine_alignment_anchor(character) {
            continue;
        }
        let (grid_x, grid_y) = baseline_grid_key(character.baseline_origin, cell_size);
        let mut consensus = None;
        let mut conflicting = false;
        for offset_x in -1..=1 {
            for offset_y in -1..=1 {
                let Some(walk_indices) = buckets.get(&(grid_x + offset_x, grid_y + offset_y))
                else {
                    continue;
                };
                for &walk_index in walk_indices {
                    if !point_close(
                        walked[walk_index].baseline_origin,
                        character.baseline_origin,
                        tolerance,
                    ) {
                        continue;
                    }
                    has_walk_candidates[engine_index] = true;
                    let modified = modified_walk[walk_index];
                    if consensus.is_some_and(|state| state != modified) {
                        conflicting = true;
                    } else {
                        consensus = Some(modified);
                    }
                }
            }
        }
        if !conflicting {
            states[engine_index] = consensus;
        }
    }
    (states, has_walk_candidates)
}

fn exact_unicode_sequence(walked: &[crate::walk::WalkedChar], engine: &[PageCharSnapshot]) -> bool {
    walked.len() == engine.len()
        && walked
            .iter()
            .zip(engine)
            .all(|(walked, engine)| walked.unicode == engine.unicode)
}

const MAX_SEQUENCE_ALIGNMENT_CELLS: usize = 4_000_000;

fn sequence_engine_indices_by_walk(
    walked: &[crate::walk::WalkedChar],
    engine: &[PageCharSnapshot],
) -> Option<Vec<Option<usize>>> {
    if exact_unicode_sequence(walked, engine) {
        return Some((0..walked.len()).map(Some).collect());
    }
    let rows = walked.len().checked_add(1)?;
    let columns = engine.len().checked_add(1)?;
    let cells = rows.checked_mul(columns)?;
    if cells > MAX_SEQUENCE_ALIGNMENT_CELLS {
        return None;
    }
    let mut lengths = vec![0u32; cells];
    for walk_index in 0..walked.len() {
        for engine_index in 0..engine.len() {
            let index = (walk_index + 1) * columns + engine_index + 1;
            lengths[index] =
                if source_characters_correspond(&walked[walk_index], &engine[engine_index]) {
                    lengths[walk_index * columns + engine_index] + 1
                } else {
                    lengths[walk_index * columns + engine_index + 1]
                        .max(lengths[(walk_index + 1) * columns + engine_index])
                };
        }
    }

    let mut mapping = vec![None; walked.len()];
    let mut walk_index = walked.len();
    let mut engine_index = engine.len();
    while walk_index > 0 && engine_index > 0 {
        if source_characters_correspond(&walked[walk_index - 1], &engine[engine_index - 1])
            && lengths[walk_index * columns + engine_index]
                == lengths[(walk_index - 1) * columns + engine_index - 1] + 1
        {
            mapping[walk_index - 1] = Some(engine_index - 1);
            walk_index -= 1;
            engine_index -= 1;
        } else if lengths[(walk_index - 1) * columns + engine_index]
            >= lengths[walk_index * columns + engine_index - 1]
        {
            walk_index -= 1;
        } else {
            engine_index -= 1;
        }
    }
    Some(mapping)
}

fn source_characters_correspond(
    walked: &crate::walk::WalkedChar,
    engine: &PageCharSnapshot,
) -> bool {
    walked.unicode == engine.unicode
        || is_pdfium_utf16_surrogate(walked, engine)
        || PDFIUM_LIGATURE_FIRST_COMPONENTS.contains(&(
            walked.unicode.unwrap_or('\0'),
            engine.unicode.unwrap_or('\0'),
        ))
}

fn extend_engine_owners_by_contiguous_sequence(
    walked: &[crate::walk::WalkedChar],
    engine: &[PageCharSnapshot],
    owners: &mut [Option<usize>],
    baseline_tolerance_pt: f64,
) {
    if owners.len() != engine.len() {
        return;
    }
    let anchors = owners
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(engine_index, walk_index)| {
            walk_index.map(|walk_index| (engine_index, walk_index))
        })
        .collect::<Vec<_>>();
    let mut claimed_walk = vec![false; walked.len()];
    for &(_, walk_index) in &anchors {
        if let Some(claimed) = claimed_walk.get_mut(walk_index) {
            *claimed = true;
        }
    }

    let candidates_correspond = |walk_index: usize, engine_index: usize| {
        source_characters_correspond(&walked[walk_index], &engine[engine_index])
            && (walked[walk_index].baseline_origin.y - engine[engine_index].baseline_origin.y).abs()
                <= baseline_tolerance_pt
    };

    for (engine_anchor, walk_anchor) in anchors {
        let mut engine_index = engine_anchor;
        let mut walk_index = walk_anchor;
        while engine_index + 1 < engine.len() && walk_index + 1 < walked.len() {
            let next_engine = engine_index + 1;
            let next_walk = walk_index + 1;
            if owners[next_engine].is_some()
                || claimed_walk[next_walk]
                || !candidates_correspond(next_walk, next_engine)
            {
                break;
            }
            owners[next_engine] = Some(next_walk);
            claimed_walk[next_walk] = true;
            engine_index = next_engine;
            walk_index = next_walk;
        }

        let mut engine_index = engine_anchor;
        let mut walk_index = walk_anchor;
        while engine_index > 0 && walk_index > 0 {
            let previous_engine = engine_index - 1;
            let previous_walk = walk_index - 1;
            if owners[previous_engine].is_some()
                || claimed_walk[previous_walk]
                || !candidates_correspond(previous_walk, previous_engine)
            {
                break;
            }
            owners[previous_engine] = Some(previous_walk);
            claimed_walk[previous_walk] = true;
            engine_index = previous_engine;
            walk_index = previous_walk;
        }
    }
}

fn inherit_pdfium_utf16_surrogate_owners(
    walked: &[crate::walk::WalkedChar],
    engine: &[PageCharSnapshot],
    owners: &mut [Option<usize>],
) {
    for engine_index in 1..engine.len() {
        if owners[engine_index].is_some() {
            continue;
        }
        let Some(walk_index) = owners[engine_index - 1] else {
            continue;
        };
        let Some(unicode) = walked[walk_index].unicode else {
            continue;
        };
        let mut units = [0u16; 2];
        let encoded = unicode.encode_utf16(&mut units);
        if encoded.len() == 2
            && engine[engine_index - 1].unicode_value == u32::from(encoded[0])
            && engine[engine_index].unicode_value == u32::from(encoded[1])
        {
            owners[engine_index] = Some(walk_index);
        }
    }
}

fn inherit_pdfium_ligature_expansion_owners(
    walked: &[crate::walk::WalkedChar],
    engine: &[PageCharSnapshot],
    owners: &mut [Option<usize>],
) {
    if owners.len() != engine.len() {
        return;
    }
    for (walk_index, walked_character) in walked.iter().enumerate() {
        let Some(expansion) = walked_character.unicode.and_then(pdfium_ligature_expansion) else {
            continue;
        };
        let mut candidate_starts = BTreeSet::new();
        for (engine_index, owner) in owners.iter().enumerate() {
            if *owner != Some(walk_index) {
                continue;
            }
            for (component_index, component) in expansion.iter().enumerate() {
                if engine[engine_index].unicode != Some(*component)
                    || engine_index < component_index
                {
                    continue;
                }
                let start = engine_index - component_index;
                let end = start + expansion.len();
                if end <= engine.len()
                    && engine[start..end]
                        .iter()
                        .map(|character| character.unicode)
                        .eq(expansion.iter().copied().map(Some))
                    && owners[start..end]
                        .iter()
                        .all(|owner| owner.is_none() || *owner == Some(walk_index))
                {
                    candidate_starts.insert(start);
                }
            }
        }
        if candidate_starts.len() != 1 {
            continue;
        }
        let start = *candidate_starts
            .first()
            .expect("one ligature expansion candidate exists");
        for owner in &mut owners[start..start + expansion.len()] {
            *owner = Some(walk_index);
        }
    }
}

fn inherit_unique_explanation_owners(
    walked: &[crate::walk::WalkedChar],
    engine: &[PageCharSnapshot],
    owners: &mut [Option<usize>],
    tolerance: f64,
) {
    if owners.len() != engine.len() {
        return;
    }
    let mut walk_matched = vec![false; walked.len()];
    let engine_matched = owners
        .iter()
        .map(|owner| {
            if let Some(walk_index) = owner {
                if let Some(matched) = walk_matched.get_mut(*walk_index) {
                    *matched = true;
                }
                true
            } else {
                false
            }
        })
        .collect::<Vec<_>>();
    let (pairs, _, _) =
        match_unique_explanation_edges(walked, engine, tolerance, &walk_matched, &engine_matched);
    for pair in pairs {
        owners[pair.engine_index] = Some(pair.walk_index);
    }
}

fn validate_mixed_output_characters(
    page_index: usize,
    retained: &[ExpectedOutputCharacter],
    typeset: &[TypesetCharacter],
    actual: &[PageCharSnapshot],
    tolerance: f64,
) -> Result<()> {
    let mut matched = vec![false; actual.len()];
    for expected_character in retained {
        if expected_character
            .unicode
            .is_some_and(|unicode| PDFIUM_C0_EXTRACTION_MARKERS.contains(&u32::from(unicode)))
        {
            continue;
        }
        let owners = match_output_character(expected_character, actual, &matched, tolerance)
            .ok_or_else(|| {
                let unicode = expected_character
                    .unicode
                    .map(|character| format!("U+{:04X}", u32::from(character)))
                    .unwrap_or_else(|| "unresolved Unicode".to_owned());
                let observed = actual
                    .iter()
                    .enumerate()
                    .filter(|(index, actual_character)| {
                        !matched[*index]
                            && output_unicode_matches(
                                expected_character.unicode,
                                actual_character,
                            )
                    })
                    .take(8)
                    .map(|(_, actual_character)| {
                        format!(
                            "({:.6}, {:.6})",
                            actual_character.baseline_origin.x,
                            actual_character.baseline_origin.y
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                output_mismatch(format!(
                    "output page {} is missing preserved character {} at ({:.6}, {:.6}); unmatched observed baselines: [{}]",
                    page_index + 1,
                    unicode,
                    expected_character.baseline_origin.x,
                    expected_character.baseline_origin.y,
                    observed,
                ))
            })?;
        for owner in owners {
            matched[owner] = true;
        }
    }
    let mut proven_typeset_ink: Vec<ExpectedOutputCharacter> = Vec::new();
    for character in typeset {
        let expected_character = ExpectedOutputCharacter {
            unicode: Some(character.unicode),
            baseline_origin: character.baseline_origin,
        };
        let Some(owners) = match_output_character(&expected_character, actual, &matched, tolerance)
        else {
            // PDFium's extraction view may omit whitespace that is present in the content stream.
            if character.unicode.is_whitespace() {
                continue;
            }
            if proven_typeset_ink.iter().any(|proven| {
                proven.unicode == expected_character.unicode
                    && point_close(
                        proven.baseline_origin,
                        expected_character.baseline_origin,
                        tolerance,
                    )
            }) {
                continue;
            }
            let observed = actual
                .iter()
                .enumerate()
                .filter(|(index, actual_character)| {
                    !matched[*index]
                        && output_unicode_matches(Some(character.unicode), actual_character)
                })
                .take(8)
                .map(|(_, actual_character)| {
                    format!(
                        "({:.6}, {:.6})",
                        actual_character.baseline_origin.x, actual_character.baseline_origin.y
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(output_mismatch(format!(
                "output page {} is missing typeset character U+{:04X} at ({:.6}, {:.6}); unmatched observed baselines: [{}]",
                page_index + 1,
                u32::from(character.unicode),
                character.baseline_origin.x,
                character.baseline_origin.y,
                observed,
            )));
        };
        for owner in owners {
            matched[owner] = true;
        }
        proven_typeset_ink.push(expected_character);
    }
    if let Some(character) = actual
        .iter()
        .zip(&matched)
        .find_map(|(character, matched)| {
            (!matched
                && !is_pdfium_c0_extraction_marker(character)
                && !retained_marker_matches_output_hyphen(retained, character, tolerance))
            .then_some(character)
        })
    {
        return Err(output_mismatch(format!(
            "output page {} has unexpected extracted character U+{:04X} at ({:.6}, {:.6})",
            page_index + 1,
            character.unicode_value,
            character.baseline_origin.x,
            character.baseline_origin.y,
        )));
    }
    Ok(())
}

fn match_output_character(
    expected: &ExpectedOutputCharacter,
    actual: &[PageCharSnapshot],
    matched: &[bool],
    tolerance: f64,
) -> Option<Vec<usize>> {
    let scalar_match = actual
        .iter()
        .enumerate()
        .filter(|(index, actual_character)| {
            !matched[*index]
                && output_unicode_matches(expected.unicode, actual_character)
                && point_close(
                    expected.baseline_origin,
                    actual_character.baseline_origin,
                    tolerance,
                )
        })
        .min_by(|(_, left), (_, right)| {
            (left.unicode != expected.unicode)
                .cmp(&(right.unicode != expected.unicode))
                .then_with(|| {
                    point_distance_squared(expected.baseline_origin, left.baseline_origin)
                        .total_cmp(&point_distance_squared(
                            expected.baseline_origin,
                            right.baseline_origin,
                        ))
                })
        })
        .map(|(index, _)| vec![index]);
    if scalar_match.is_some() {
        return scalar_match;
    }

    let unicode = expected.unicode?;
    let mut units = [0u16; 2];
    let encoded = unicode.encode_utf16(&mut units);
    if encoded.len() != 2 {
        return None;
    }
    actual
        .windows(2)
        .enumerate()
        .filter(|(index, pair)| {
            !matched[*index]
                && !matched[*index + 1]
                && pair[0].unicode_value == u32::from(encoded[0])
                && pair[1].unicode_value == u32::from(encoded[1])
                && point_close(expected.baseline_origin, pair[0].baseline_origin, tolerance)
                && point_close(expected.baseline_origin, pair[1].baseline_origin, tolerance)
        })
        .min_by(|(_, left), (_, right)| {
            point_distance_squared(expected.baseline_origin, left[0].baseline_origin).total_cmp(
                &point_distance_squared(expected.baseline_origin, right[0].baseline_origin),
            )
        })
        .map(|(index, _)| vec![index, index + 1])
}

fn output_unicode_matches(expected: Option<char>, actual: &PageCharSnapshot) -> bool {
    actual.unicode == expected || (expected == Some('-') && actual.unicode_value == 2)
}

fn retained_marker_matches_output_hyphen(
    retained: &[ExpectedOutputCharacter],
    actual: &PageCharSnapshot,
    tolerance: f64,
) -> bool {
    actual.unicode == Some('-')
        && retained.iter().any(|expected| {
            expected.unicode == Some('\u{2}')
                && point_close(expected.baseline_origin, actual.baseline_origin, tolerance)
        })
}

fn point_distance_squared(left: il::Point, right: il::Point) -> f64 {
    let delta_x = left.x - right.x;
    let delta_y = left.y - right.y;
    delta_x.mul_add(delta_x, delta_y * delta_y)
}

fn validate_typeset_characters(
    page_index: usize,
    expected: &[TypesetCharacter],
    actual: &[PageCharSnapshot],
    tolerance: f64,
) -> Result<()> {
    validate_mixed_output_characters(page_index, &[], expected, actual, tolerance)
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

const PDFIUM_C0_EXTRACTION_MARKERS: std::ops::RangeInclusive<u32> = 0x0000..=0x001F;
const PDFIUM_LIGATURE_FIRST_COMPONENTS: [(char, char); 7] = [
    ('\u{FB00}', 'f'),
    ('\u{FB01}', 'f'),
    ('\u{FB02}', 'f'),
    ('\u{FB03}', 'f'),
    ('\u{FB04}', 'f'),
    ('\u{FB05}', 's'),
    ('\u{FB06}', 's'),
];

fn pdfium_ligature_expansion(character: char) -> Option<&'static [char]> {
    match character {
        '\u{FB00}' => Some(&['f', 'f']),
        '\u{FB01}' => Some(&['f', 'i']),
        '\u{FB02}' => Some(&['f', 'l']),
        '\u{FB03}' => Some(&['f', 'f', 'i']),
        '\u{FB04}' => Some(&['f', 'f', 'l']),
        '\u{FB05}' | '\u{FB06}' => Some(&['s', 't']),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct BaselineMatch {
    walk_index: usize,
    engine_index: usize,
}

#[derive(Debug, Default)]
struct AlignmentCounts {
    extraction_equivalent: usize,
    explained: usize,
    strong_unicode_conflict: usize,
    weak_unicode_conflict: usize,
    unresolved_unicode: usize,
    walk_only: usize,
    engine_only: usize,
    residual: usize,
}

#[derive(Debug, Default)]
struct BaselineResiduals {
    count: usize,
    max_delta_x_pt: f64,
    max_delta_y_pt: f64,
}

impl AlignmentCounts {
    fn has_diagnostic(&self) -> bool {
        self.strong_unicode_conflict
            + self.weak_unicode_conflict
            + self.unresolved_unicode
            + self.walk_only
            + self.engine_only
            + self.residual
            > 0
    }
}

fn validate_character_alignment(
    page_index: usize,
    page_geometry: PageGeometry,
    walked: &[crate::walk::WalkedChar],
    engine: &[crate::engine::PageCharSnapshot],
    tolerance: f64,
    diagnostics: &mut Diagnostics,
) -> CharacterAlignment {
    let mut alignment = CharacterAlignment {
        engine_indices_by_walk: vec![None; walked.len()],
        weak_unicode_conflicts: BTreeSet::new(),
    };
    if !tolerance.is_finite() || tolerance < 0.0 {
        diagnostics.push(Diagnostic::EngineCharacterMismatch {
            page_index,
            character_index: None,
            walked_character_count: walked.len(),
            engine_character_count: engine.len(),
            walked_unicode: None,
            engine_unicode: None,
        });
        return alignment;
    }

    let (pairs, walk_candidate_counts, engine_candidate_counts) =
        match_baseline_multisets(walked, engine, tolerance);
    let mut walk_matched = vec![false; walked.len()];
    let mut engine_matched = vec![false; engine.len()];
    let mut counts = AlignmentCounts::default();

    for pair in pairs {
        walk_matched[pair.walk_index] = true;
        engine_matched[pair.engine_index] = true;
        alignment.engine_indices_by_walk[pair.walk_index] = Some(pair.engine_index);
        let walk = &walked[pair.walk_index];
        let engine_character = &engine[pair.engine_index];
        let ignored = walk.unicode.is_some_and(char::is_whitespace)
            || engine_character.unicode.is_some_and(char::is_whitespace)
            || !walk.visible;
        if walk.unicode == engine_character.unicode {
            if walk.unicode.is_none() && !ignored {
                counts.unresolved_unicode += 1;
            }
            continue;
        }
        if ignored || is_extraction_view_equivalent(walk, engine_character) {
            counts.extraction_equivalent += 1;
            continue;
        }
        match walk.unicode_provenance {
            UnicodeProvenance::ToUnicode
            | UnicodeProvenance::EmbeddedFontCmap
            | UnicodeProvenance::EmbeddedType1Encoding => {
                counts.strong_unicode_conflict += 1;
            }
            UnicodeProvenance::SimpleEncoding | UnicodeProvenance::DifferencesAgl => {
                counts.weak_unicode_conflict += 1;
                alignment.weak_unicode_conflicts.insert(pair.walk_index);
            }
            UnicodeProvenance::Unresolved => counts.unresolved_unicode += 1,
        }
    }

    let (explanation_pairs, explanation_walk_candidate_counts, explanation_engine_candidate_counts) =
        match_unique_explanation_edges(walked, engine, tolerance, &walk_matched, &engine_matched);
    // Explanation suppresses a false engine-only residue but never grants the
    // per-character engine geometry link used for tight-box adoption.
    for pair in explanation_pairs {
        walk_matched[pair.walk_index] = true;
        engine_matched[pair.engine_index] = true;
        counts.explained += 1;
    }

    for (index, character) in walked.iter().enumerate() {
        if walk_matched[index] {
            continue;
        }
        if character.unicode.is_some_and(char::is_whitespace) || !character.visible {
            counts.extraction_equivalent += 1;
        } else if !valid_walk_alignment_anchor(character)
            || character.unicode.is_none()
            || walk_candidate_counts[index] > 0
            || explanation_walk_candidate_counts[index] > 0
        {
            counts.residual += 1;
        } else {
            counts.walk_only += 1;
        }
    }

    for (index, character) in engine.iter().enumerate() {
        if engine_matched[index] {
            continue;
        }
        if engine_character_is_outside_page(character, page_geometry)
            || character.unicode.is_some_and(char::is_whitespace)
            || is_pdfium_c0_extraction_marker(character)
        {
            counts.extraction_equivalent += 1;
        } else if !valid_engine_alignment_anchor(character)
            || character.unicode.is_none()
            || !rect_is_finite(character.tight_box)
            || engine_candidate_counts[index] > 0
            || explanation_engine_candidate_counts[index] > 0
        {
            counts.residual += 1;
        } else {
            counts.engine_only += 1;
        }
    }

    let baseline_residuals = collect_residual_baselines(
        page_index,
        walked,
        engine,
        tolerance,
        &walk_matched,
        &engine_matched,
        diagnostics,
    );
    if counts.has_diagnostic() || baseline_residuals.count > 0 {
        diagnostics.push(Diagnostic::EngineCharacterAlignment {
            page_index,
            walked_character_count: walked.len(),
            engine_character_count: engine.len(),
            extraction_equivalent_count: counts.extraction_equivalent,
            explained_count: counts.explained,
            strong_unicode_conflict_count: counts.strong_unicode_conflict,
            weak_unicode_conflict_count: counts.weak_unicode_conflict,
            unresolved_unicode_count: counts.unresolved_unicode,
            walk_only_count: counts.walk_only,
            engine_only_count: counts.engine_only,
            residual_count: counts.residual,
            baseline_residual_count: baseline_residuals.count,
            baseline_residual_max_delta_x_pt: baseline_residuals.max_delta_x_pt,
            baseline_residual_max_delta_y_pt: baseline_residuals.max_delta_y_pt,
        });
    }
    alignment
}

fn match_baseline_multisets(
    walked: &[crate::walk::WalkedChar],
    engine: &[crate::engine::PageCharSnapshot],
    tolerance: f64,
) -> (Vec<BaselineMatch>, Vec<usize>, Vec<usize>) {
    let cell_size = tolerance.max(f64::EPSILON);
    let mut buckets = HashMap::<(i64, i64), Vec<usize>>::new();
    for (index, character) in engine.iter().enumerate() {
        if valid_engine_alignment_anchor(character) {
            buckets
                .entry(baseline_grid_key(character.baseline_origin, cell_size))
                .or_default()
                .push(index);
        }
    }

    let mut candidates = vec![Vec::<(usize, f64)>::new(); walked.len()];
    let mut engine_candidate_counts = vec![0usize; engine.len()];
    for (walk_index, character) in walked.iter().enumerate() {
        if !valid_walk_alignment_anchor(character) {
            continue;
        }
        let (grid_x, grid_y) = baseline_grid_key(character.baseline_origin, cell_size);
        for offset_x in -1..=1 {
            for offset_y in -1..=1 {
                let Some(indices) = buckets.get(&(grid_x + offset_x, grid_y + offset_y)) else {
                    continue;
                };
                for &engine_index in indices {
                    let engine_character = &engine[engine_index];
                    let delta_x =
                        (character.baseline_origin.x - engine_character.baseline_origin.x).abs();
                    let delta_y =
                        (character.baseline_origin.y - engine_character.baseline_origin.y).abs();
                    if delta_x <= tolerance && delta_y <= tolerance {
                        let distance = delta_x.mul_add(delta_x, delta_y * delta_y);
                        candidates[walk_index].push((engine_index, distance));
                        engine_candidate_counts[engine_index] += 1;
                    }
                }
            }
        }
    }

    let walk_candidate_counts = candidates.iter().map(Vec::len).collect::<Vec<_>>();
    let mut edges = candidates
        .iter()
        .enumerate()
        .flat_map(|(walk_index, values)| {
            values
                .iter()
                .map(move |&(engine_index, distance)| (walk_index, engine_index, distance))
        })
        .collect::<Vec<_>>();
    edges.sort_by(
        |(left_walk, left_engine, left_distance), (right_walk, right_engine, right_distance)| {
            alignment_match_rank(&walked[*left_walk], &engine[*left_engine])
                .cmp(&alignment_match_rank(
                    &walked[*right_walk],
                    &engine[*right_engine],
                ))
                .then_with(|| left_distance.total_cmp(right_distance))
                .then_with(|| left_walk.cmp(right_walk))
                .then_with(|| left_engine.cmp(right_engine))
        },
    );

    let mut walk_matched = vec![false; walked.len()];
    let mut engine_matched = vec![false; engine.len()];
    let mut pairs = Vec::new();
    for (walk_index, engine_index, _) in edges {
        if walk_matched[walk_index] || engine_matched[engine_index] {
            continue;
        }
        walk_matched[walk_index] = true;
        engine_matched[engine_index] = true;
        pairs.push(BaselineMatch {
            walk_index,
            engine_index,
        });
    }
    (pairs, walk_candidate_counts, engine_candidate_counts)
}

fn match_unique_explanation_edges(
    walked: &[crate::walk::WalkedChar],
    engine: &[crate::engine::PageCharSnapshot],
    tolerance: f64,
    walk_matched: &[bool],
    engine_matched: &[bool],
) -> (Vec<BaselineMatch>, Vec<usize>, Vec<usize>) {
    let cell_size = tolerance.max(f64::EPSILON);
    let mut buckets = HashMap::<(i64, i64), Vec<usize>>::new();
    for (index, character) in engine.iter().enumerate() {
        if !engine_matched[index] && valid_engine_alignment_anchor(character) {
            buckets
                .entry(baseline_grid_key(character.baseline_origin, cell_size))
                .or_default()
                .push(index);
        }
    }

    let mut candidates = vec![Vec::<usize>::new(); walked.len()];
    let mut engine_candidate_counts = vec![0usize; engine.len()];
    for (walk_index, character) in walked.iter().enumerate() {
        if walk_matched[walk_index] || !valid_walk_explanation_anchor(character) {
            continue;
        }
        let (grid_x, grid_y) = baseline_grid_key(character.baseline_origin, cell_size);
        for offset_x in -1..=1 {
            for offset_y in -1..=1 {
                let Some(indices) = buckets.get(&(grid_x + offset_x, grid_y + offset_y)) else {
                    continue;
                };
                for &engine_index in indices {
                    let engine_character = &engine[engine_index];
                    let delta_x =
                        (character.baseline_origin.x - engine_character.baseline_origin.x).abs();
                    let delta_y =
                        (character.baseline_origin.y - engine_character.baseline_origin.y).abs();
                    if delta_x <= tolerance && delta_y <= tolerance {
                        candidates[walk_index].push(engine_index);
                        engine_candidate_counts[engine_index] += 1;
                    }
                }
            }
        }
    }

    let walk_candidate_counts = candidates.iter().map(Vec::len).collect::<Vec<_>>();
    let pairs = candidates
        .iter()
        .enumerate()
        .filter_map(|(walk_index, engine_indices)| {
            let [engine_index] = engine_indices.as_slice() else {
                return None;
            };
            (engine_candidate_counts[*engine_index] == 1).then_some(BaselineMatch {
                walk_index,
                engine_index: *engine_index,
            })
        })
        .collect();
    (pairs, walk_candidate_counts, engine_candidate_counts)
}

fn baseline_grid_key(point: crate::il::Point, cell_size: f64) -> (i64, i64) {
    (
        (point.x / cell_size).floor() as i64,
        (point.y / cell_size).floor() as i64,
    )
}

fn alignment_match_rank(
    walk: &crate::walk::WalkedChar,
    engine: &crate::engine::PageCharSnapshot,
) -> u8 {
    if walk.unicode == engine.unicode {
        0
    } else if walk.unicode.is_some_and(char::is_whitespace)
        || engine.unicode.is_some_and(char::is_whitespace)
        || !walk.visible
        || is_extraction_view_equivalent(walk, engine)
    {
        1
    } else {
        2
    }
}

fn valid_walk_alignment_anchor(character: &crate::walk::WalkedChar) -> bool {
    character.locatable
        && character.text_transform == TextTransform::Upright
        && character.unicode.is_some()
        && character.advance.is_finite()
        && character.advance > 0.0
        && character.baseline_origin.x.is_finite()
        && character.baseline_origin.y.is_finite()
}

fn valid_walk_explanation_anchor(character: &crate::walk::WalkedChar) -> bool {
    character.locatable
        && (character.text_transform != TextTransform::Upright
            || character.unicode.is_none()
            || !character.advance.is_finite()
            || character.advance <= 0.0)
        && character.baseline_origin.x.is_finite()
        && character.baseline_origin.y.is_finite()
}

fn valid_engine_alignment_anchor(character: &crate::engine::PageCharSnapshot) -> bool {
    character.baseline_origin.x.is_finite() && character.baseline_origin.y.is_finite()
}

fn is_extraction_view_equivalent(
    walk: &crate::walk::WalkedChar,
    engine: &crate::engine::PageCharSnapshot,
) -> bool {
    is_pdfium_c0_extraction_marker(engine)
        || is_pdfium_utf16_surrogate(walk, engine)
        || PDFIUM_LIGATURE_FIRST_COMPONENTS
            .contains(&(walk.unicode.unwrap_or('\0'), engine.unicode.unwrap_or('\0')))
}

fn is_pdfium_c0_extraction_marker(character: &crate::engine::PageCharSnapshot) -> bool {
    PDFIUM_C0_EXTRACTION_MARKERS.contains(&character.unicode_value)
}

fn is_pdfium_utf16_surrogate(
    walk: &crate::walk::WalkedChar,
    engine: &crate::engine::PageCharSnapshot,
) -> bool {
    let Some(unicode) = walk.unicode else {
        return false;
    };
    let mut units = [0u16; 2];
    let encoded = unicode.encode_utf16(&mut units);
    encoded.len() == 2 && u32::from(encoded[0]) == engine.unicode_value
}

fn rect_is_finite(rect: Rect) -> bool {
    rect.left.is_finite()
        && rect.bottom.is_finite()
        && rect.right.is_finite()
        && rect.top.is_finite()
}

fn engine_character_is_outside_page(
    character: &crate::engine::PageCharSnapshot,
    geometry: PageGeometry,
) -> bool {
    rect_is_finite(character.tight_box)
        && geometry.width.is_finite()
        && geometry.height.is_finite()
        && (character.tight_box.right <= 0.0
            || character.tight_box.left >= geometry.width
            || character.tight_box.top <= 0.0
            || character.tight_box.bottom >= geometry.height)
}

fn collect_residual_baselines(
    page_index: usize,
    walked: &[crate::walk::WalkedChar],
    engine: &[crate::engine::PageCharSnapshot],
    tolerance: f64,
    walk_matched: &[bool],
    engine_matched: &[bool],
    diagnostics: &mut Diagnostics,
) -> BaselineResiduals {
    let mut residuals = BaselineResiduals::default();
    for (index, walk) in walked.iter().enumerate() {
        let Some(engine_character) = engine.get(index) else {
            continue;
        };
        if walk_matched[index]
            || engine_matched[index]
            || walk.unicode != engine_character.unicode
            || !valid_walk_alignment_anchor(walk)
            || !valid_engine_alignment_anchor(engine_character)
        {
            continue;
        }
        let delta_x = (walk.baseline_origin.x - engine_character.baseline_origin.x).abs();
        let delta_y = (walk.baseline_origin.y - engine_character.baseline_origin.y).abs();
        if delta_x > tolerance || delta_y > tolerance {
            residuals.count += 1;
            residuals.max_delta_x_pt = residuals.max_delta_x_pt.max(delta_x);
            residuals.max_delta_y_pt = residuals.max_delta_y_pt.max(delta_y);
            diagnostics.push_debug(Diagnostic::EngineBaselineMismatch {
                page_index,
                character_index: index,
                delta_x_pt: delta_x,
                delta_y_pt: delta_y,
            });
        }
    }
    residuals
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
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};

    use sha2::{Digest, Sha256};

    use crate::PipelineConfig;
    use crate::engine::{
        LayoutDetector, LayoutRegion, OnnxLayoutDetector, PageCharSnapshot, PdfInspector,
        Rasterizer, RgbaImage, SingleLineLayoutDetector,
    };
    use crate::event::{
        CacheStatus, DiagnosticId, EventKind, NoopEventSink, PageDegradeReason, RecordingEventSink,
    };
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

    /// Dev-only recovery tool for an adjudicated policy change that removes prepared requests.
    /// See `docs/agents/term-cache-migration.md` before running this ignored test.
    #[test]
    #[ignore = "requires archived BERT PDF/cache plus pinned PDFium and ONNX assets"]
    fn migrate_bert_term_cache_after_author_geometry_policy() {
        let source_cache = required_path_env("MIMUS_TERM_MIGRATION_SOURCE_CACHE");
        let target_cache = required_path_env("MIMUS_TERM_MIGRATION_TARGET_CACHE");
        let paper = required_path_env("MIMUS_TERM_MIGRATION_PDF");
        let layout_model = required_path_env("MIMUS_TERM_MIGRATION_LAYOUT_MODEL");
        let model = required_string_env("MIMUS_TERM_MIGRATION_MODEL");
        let target_language = required_string_env("MIMUS_TERM_MIGRATION_TARGET_LANGUAGE");
        let migration_date = required_string_env("MIMUS_TERM_MIGRATION_DATE");

        assert_eq!(model, "m35-proxy-model");
        assert_eq!(target_language, "zh-CN");
        assert_eq!(
            target_cache.file_name().unwrap(),
            "05-bert-m3-7-author-geometry.redb"
        );
        assert_ne!(
            std::fs::canonicalize(&source_cache).unwrap(),
            std::fs::canonicalize(&target_cache).unwrap(),
            "the archive and writable migration target must be different files"
        );
        assert!(
            std::fs::metadata(&source_cache)
                .unwrap()
                .permissions()
                .readonly(),
            "the source archive must remain read-only"
        );
        let source_before = std::fs::read(&source_cache).unwrap();
        let target_before = std::fs::read(&target_cache).unwrap();
        assert_eq!(
            target_before, source_before,
            "start from a byte-identical cache copy before making it writable"
        );
        let source_sha256 = sha256_hex_for_migration(&source_before);

        let pdfium = crate::engine::pdfium::PdfiumEngine::from_environment().unwrap();
        let detector = OnnxLayoutDetector::from_file(&layout_model).unwrap();
        let translator = crate::translate::NoneTranslator;
        let events = NoopEventSink;
        let mut document = Document::for_translation(&paper, target_cache.with_extension("pdf"));
        let context = PassContext {
            engine: &pdfium,
            layout_detector: &detector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: PipelineConfig {
                target_language: target_language.clone(),
                ..PipelineConfig::default()
            },
        };
        for pass in [parse, scan_detect, layout, paragraph_find] {
            pass(&mut document, &context).unwrap();
        }

        let paragraph_find_il = document.il.clone();
        styles_and_formulas(&mut document, &context).unwrap();
        let new_requests = term_extraction_requests(&document);
        let new_document_text = term_extraction_document_text(&document);

        document.il = paragraph_find_il;
        for reading_order in [11, 12] {
            let paragraph = document.il.pages[0]
                .paragraphs
                .iter_mut()
                .find(|paragraph| paragraph.reading_order == reading_order)
                .unwrap_or_else(|| panic!("missing BERT page-0 paragraph {reading_order}"));
            assert!(paragraph_has_only_label(
                paragraph,
                LayoutLabel::FallbackLine
            ));
            assert!(paragraph.chars().iter().all(|character| {
                character
                    .layout
                    .is_some_and(|layout| layout.policy == TranslationPolicy::Passthrough)
            }));
            for character in paragraph_chars_mut(paragraph) {
                character.layout.as_mut().unwrap().policy = TranslationPolicy::Translate;
            }
        }
        styles_and_formulas(&mut document, &context).unwrap();
        let old_requests = term_extraction_requests(&document);
        let old_document_text = term_extraction_document_text(&document);

        let removed = old_requests
            .iter()
            .filter(|(key, request)| new_requests.get(key) != Some(request))
            .map(|(key, request)| (*key, request.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            removed.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            [(0, 11), (0, 12)]
        );
        assert_eq!(old_requests.len(), new_requests.len() + 2);
        assert!(
            new_requests
                .iter()
                .all(|(key, value)| old_requests.get(key) == Some(value))
        );

        let old_key = crate::translate::cache::TermExtractionCacheKey::new(
            &old_document_text,
            &model,
            &target_language,
            crate::translate::TERMS_PROMPT_VERSION,
        );
        let new_key = crate::translate::cache::TermExtractionCacheKey::new(
            &new_document_text,
            &model,
            &target_language,
            crate::translate::TERMS_PROMPT_VERSION,
        );
        assert_ne!(old_key, new_key);
        let migrated =
            crate::translate::cache::migrate_unique_terms_entry(&target_cache, &old_key, &new_key)
                .unwrap();
        let glossary = crate::translate::Glossary::from_toml(&migrated.value).unwrap();
        let lowered = migrated.value.to_ascii_lowercase();
        for forbidden in ["devlin", "chang", "kenton", "toutanova", "google ai"] {
            assert!(
                !lowered.contains(forbidden),
                "old glossary unexpectedly contains {forbidden}"
            );
        }

        assert_eq!(
            sha256_hex_for_migration(&std::fs::read(&source_cache).unwrap()),
            source_sha256,
            "the source archive changed during migration"
        );
        let target_sha256 = sha256_hex_for_migration(&std::fs::read(&target_cache).unwrap());
        let sidecar = target_cache.with_extension("provenance.json");
        let removed = removed
            .into_iter()
            .map(|((page_index, reading_order), request)| {
                serde_json::json!({
                    "page_index": page_index,
                    "reading_order": reading_order,
                    "request_sha256": sha256_hex_for_migration(request.as_bytes()),
                })
            })
            .collect::<Vec<_>>();
        let provenance = serde_json::json!({
            "schema_version": 1,
            "operation": "copy_unique_extracted_glossary_entry",
            "date": migration_date,
            "source_cache_sha256": source_sha256,
            "target_cache_sha256": target_sha256,
            "old_key": old_key.hex(),
            "new_key": new_key.hex(),
            "old_document_text": {
                "sha256": sha256_hex_for_migration(old_document_text.as_bytes()),
                "length_bytes": old_document_text.len(),
            },
            "new_document_text": {
                "sha256": sha256_hex_for_migration(new_document_text.as_bytes()),
                "length_bytes": new_document_text.len(),
            },
            "removed_requests": removed,
            "glossary_fingerprint": glossary.fingerprint(),
            "model": model,
            "target_language": target_language,
            "prompt_version": crate::translate::TERMS_PROMPT_VERSION,
            "model_calls": 0,
        });
        let mut encoded = serde_json::to_vec_pretty(&provenance).unwrap();
        encoded.push(b'\n');
        std::fs::write(&sidecar, encoded).unwrap();
    }

    fn term_extraction_requests(document: &Document) -> BTreeMap<(usize, usize), String> {
        document
            .prepared_translations
            .iter()
            .filter(|(_, prepared)| !prepared.is_local_identity())
            .filter(|(_, prepared)| !prepared.request_text().is_empty())
            .map(|(key, prepared)| (*key, prepared.request_text().to_owned()))
            .collect()
    }

    fn required_path_env(name: &str) -> PathBuf {
        PathBuf::from(required_string_env(name))
    }

    fn required_string_env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("set {name} before running the migration"))
    }

    fn sha256_hex_for_migration(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn missing_normal_xref_object_reports_its_id_and_offset() {
        let mut pdf = LopdfDocument::with_version("1.7");
        pdf.reference_table.entries.insert(
            12,
            XrefEntry::Normal {
                offset: 4096,
                generation: 3,
            },
        );

        let error = validate_xref_objects(&pdf).unwrap_err();
        assert_eq!(error.reason(), ErrorReason::Input(InputReason::PdfParse));
        assert_eq!(
            error.input_detail(),
            Some(InputErrorDetail::ObjectSyntax {
                objid: [12, 3],
                offset: 4096,
            })
        );
    }

    #[test]
    fn page_tree_validation_rejects_missing_and_non_page_kids() {
        for kid in [Object::Reference((99, 0)), Object::Reference((5, 0))] {
            let mut pdf = LopdfDocument::load(fixture()).unwrap();
            pdf.get_object_mut((2, 0))
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .get_mut(b"Kids")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .push(kid);

            let error = validated_page_ids(&pdf).unwrap_err();
            assert_eq!(
                error.reason(),
                crate::error::ErrorReason::Input(InputReason::PdfParse)
            );
        }
    }

    #[test]
    fn parse_validation_rejects_a_wrong_object_stream_count() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../corpus/fixtures/mal-parse-08-broken-objstm/mal-parse-08-broken-objstm.pdf",
        );
        let pdf = LopdfDocument::load(path).unwrap();
        let error = validate_object_streams(&pdf).unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Input(InputReason::PdfParse)
        );
    }

    #[derive(Default)]
    struct FakeEngine {
        raster_calls: AtomicUsize,
        corrupt_raster_after_bytes: Option<usize>,
        characters: Option<Vec<PageCharSnapshot>>,
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
            if let Some(characters) = &self.characters {
                return Ok(characters.clone());
            }
            let origins = [72.0, 82.356, 85.896, 96.252, 105.036];
            Ok("MIMUS"
                .chars()
                .zip(origins)
                .enumerate()
                .map(|(index, (unicode, x))| PageCharSnapshot {
                    index: index as u32,
                    unicode: Some(unicode),
                    unicode_value: unicode.into(),
                    is_hyphen: None,
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
        fn translate(&self, request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(request.text.to_owned())
        }

        fn model_id(&self) -> &str {
            "none"
        }
    }

    #[derive(Default)]
    struct WrappingTranslator {
        inputs: Mutex<Vec<String>>,
    }

    impl Translator for WrappingTranslator {
        fn translate(&self, request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            self.inputs.lock().unwrap().push(request.text.to_owned());
            Ok(format!("[{}]", request.text))
        }
    }

    struct NonIdentityTranslator;

    impl Translator for NonIdentityTranslator {
        fn translate(&self, request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            Ok(format!("{}龘", request.text))
        }
    }

    struct CjkTranslator;

    impl Translator for CjkTranslator {
        fn translate(&self, _request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            Ok("MIMUS中文测试".to_owned())
        }
    }

    struct VariableFontTranslator;

    impl Translator for VariableFontTranslator {
        fn translate(&self, _request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            Ok("M中文测试".to_owned())
        }
    }

    struct StaticTranslator {
        output: &'static str,
        calls: AtomicUsize,
    }

    #[derive(Default)]
    struct GlossaryTranslator {
        term_calls: AtomicUsize,
        translation_glossaries: Mutex<Vec<crate::translate::Glossary>>,
    }

    impl Translator for GlossaryTranslator {
        fn translate(&self, request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            self.translation_glossaries
                .lock()
                .unwrap()
                .push(request.glossary.clone());
            Ok("Translated".to_owned())
        }

        fn extract_terms(
            &self,
            _request: &crate::translate::TermExtractionRequest<'_>,
        ) -> Result<crate::translate::Glossary> {
            self.term_calls.fetch_add(1, Ordering::SeqCst);
            crate::translate::Glossary::from_toml(
                "version = 1\n[[terms]]\nsource = 'attention'\ntarget = 'auto-attention'\n[[terms]]\nsource = 'model'\ntarget = 'auto-model'\n",
            )
        }
    }

    impl Translator for StaticTranslator {
        fn translate(&self, _request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.output.to_owned())
        }
    }

    #[derive(Default)]
    struct FailingTranslator {
        calls: AtomicUsize,
    }

    struct BoundedTranslator {
        first_wave: Barrier,
        concurrency: usize,
        calls: AtomicUsize,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
    }

    impl BoundedTranslator {
        fn new(concurrency: usize) -> Self {
            Self {
                first_wave: Barrier::new(concurrency),
                concurrency,
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
            }
        }
    }

    impl Translator for BoundedTranslator {
        fn translate(&self, request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(in_flight, Ordering::SeqCst);
            if call < self.concurrency {
                self.first_wave.wait();
                std::thread::sleep(std::time::Duration::from_millis(
                    ((self.concurrency - call) * 5) as u64,
                ));
            }
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(format!("translated:{}", request.text))
        }
    }

    #[derive(Default)]
    struct RetryingTranslator {
        calls: AtomicUsize,
    }

    impl Translator for RetryingTranslator {
        fn translate(&self, _request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < 2 {
                return Err(MimusError::retryable_translation(
                    crate::error::TranslationReason::BackendRejected,
                    crate::error::RetryReason::RateLimited,
                    "retry without a response body",
                ));
            }
            Ok("Translated".to_owned())
        }
    }

    struct SelectiveFailTranslator;

    impl Translator for SelectiveFailTranslator {
        fn translate(&self, request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            if request.text.starts_with('B') {
                return Err(MimusError::translation(
                    TranslationReason::TranslationFailed,
                    "injected paragraph failure",
                ));
            }
            Ok(format!("translated:{}", request.text))
        }
    }

    #[derive(Debug, Default)]
    struct RecordingSleeper {
        durations: Mutex<Vec<std::time::Duration>>,
    }

    impl crate::translate::Sleeper for RecordingSleeper {
        fn sleep(&self, duration: std::time::Duration) {
            self.durations.lock().unwrap().push(duration);
        }
    }

    impl Translator for FailingTranslator {
        fn translate(&self, _request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(MimusError::translation(
                crate::error::TranslationReason::TransportFailure,
                "injected transport failure",
            ))
        }
    }

    #[derive(Default)]
    struct FailingTermTranslator {
        term_calls: AtomicUsize,
    }

    impl Translator for FailingTermTranslator {
        fn translate(&self, request: &crate::translate::TranslationRequest<'_>) -> Result<String> {
            Ok(request.text.to_owned())
        }

        fn extract_terms(
            &self,
            _request: &crate::translate::TermExtractionRequest<'_>,
        ) -> Result<crate::translate::Glossary> {
            self.term_calls.fetch_add(1, Ordering::SeqCst);
            Err(MimusError::translation(
                crate::error::TranslationReason::TransportFailure,
                "injected term extraction failure",
            ))
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
            _page_index: usize,
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

    fn test_output_fonts() -> crate::context::OutputFonts {
        let regular = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mimus/tests/assets/fonts/MimusTestGB2312-Regular.ttf"
        ));
        let bold = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mimus/tests/assets/fonts/MimusTestGB2312-Bold.ttf"
        ));
        let fallback_regular = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mimus/tests/assets/fonts/MimusTestFallback-Regular.ttf"
        ));
        let fallback_bold = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mimus/tests/assets/fonts/MimusTestFallback-Bold.ttf"
        ));
        crate::context::OutputFonts {
            regular: crate::context::OutputFont {
                bytes: regular.to_vec(),
                postscript_name: "NotoSansSC".to_owned(),
                source: "test:regular".to_owned(),
                sha256: "510d0470ca8b77f035fe8e7143526207088c1bdad017451cf253020f72397d63"
                    .to_owned(),
            },
            bold: crate::context::OutputFont {
                bytes: bold.to_vec(),
                postscript_name: "NotoSansSC".to_owned(),
                source: "test:bold".to_owned(),
                sha256: "1a917349eb06866f5701532f0cea586d184edadbd1cfdd3f034f3a18f2ff5316"
                    .to_owned(),
            },
            fallback_regular: crate::context::OutputFont {
                bytes: fallback_regular.to_vec(),
                postscript_name: "DejaVuSans".to_owned(),
                source: "test:fallback-regular".to_owned(),
                sha256: "3634d4b65a151c61dcb82968f6a3bdc33435d062c4c69a5ea57e3db20122ac1e"
                    .to_owned(),
            },
            fallback_bold: crate::context::OutputFont {
                bytes: fallback_bold.to_vec(),
                postscript_name: "DejaVuSans-Bold".to_owned(),
                source: "test:fallback-bold".to_owned(),
                sha256: "d0f2fdc62e7cdf6e35c8b0629b19084917991603c0d51fe94109128176352b83"
                    .to_owned(),
            },
        }
    }

    fn test_variable_output_fonts() -> crate::context::OutputFonts {
        let variable = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mimus/tests/assets/fonts/MimusTestVariable.ttf"
        ));
        let mut fonts = test_output_fonts();
        for (font, source) in [
            (&mut fonts.regular, "test:variable-regular"),
            (&mut fonts.bold, "test:variable-bold"),
        ] {
            font.bytes = variable.to_vec();
            font.postscript_name = "NotoSansSC".to_owned();
            font.source = source.to_owned();
            font.sha256 =
                "a1105d5892eaad20ed1ad692827b06a7adc392f214a835740fa4d94bf5029ac5".to_owned();
        }
        fonts
    }

    fn assert_typeset_ink_is_disjoint(typeset_ink: &[Rect], retained_ink: &[Rect]) {
        assert!(!typeset_ink.is_empty(), "expected planned typeset ink");
        assert!(
            !rects_intersect_each_other(typeset_ink),
            "typeset lines or segments intersect: {typeset_ink:?}"
        );
        for typeset in typeset_ink {
            for retained in retained_ink {
                assert!(
                    intersection_area(*typeset, *retained) <= 0.0001,
                    "typeset ink {typeset:?} intersects retained ink {retained:?}"
                );
            }
        }
    }

    fn config_with_test_output_fonts() -> crate::context::PipelineConfig {
        crate::context::PipelineConfig {
            output_fonts: Some(test_output_fonts()),
            ..crate::context::PipelineConfig::default()
        }
    }

    fn inspected_fixture_chars() -> Vec<Char> {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        document.il.pages[0].paragraphs[0].chars().to_vec()
    }

    fn synthetic_text_line(template: &Char, start: f64, width: f64, baseline: f64) -> TextLine {
        let mut character = template.clone();
        character.unicode = Some('M');
        character.baseline_origin = Point {
            x: start,
            y: baseline,
        };
        character.r#box = Rect {
            left: start,
            bottom: baseline - 2.0,
            right: start + width,
            top: baseline + 8.0,
        };
        character.visual_bbox = character.r#box;
        let layout = character.layout.as_mut().unwrap();
        layout.bounds = Rect {
            left: 0.0,
            bottom: 0.0,
            right: 100.0,
            top: 120.0,
        };
        layout.label = LayoutLabel::Text;
        layout.source = LayoutSource::Model;
        layout.policy = TranslationPolicy::Translate;
        text_line(vec![PositionedChar {
            walked_index: 0,
            locatable: true,
            character,
            force_no_space_before: false,
            small_edge_character: false,
        }])
    }

    #[test]
    fn natural_paragraph_split_requires_indent_and_previous_underfill() {
        let template = inspected_fixture_chars().remove(0);
        let full_previous = synthetic_text_line(&template, 10.0, 85.0, 100.0);
        let indented = synthetic_text_line(&template, 30.0, 30.0, 88.0);
        assert_eq!(
            split_natural_paragraphs(vec![full_previous, indented.clone()], LayoutLabel::Text,)
                .len(),
            1,
            "indent alone must not split after a filled line"
        );

        let underfilled_previous = synthetic_text_line(&template, 10.0, 40.0, 100.0);
        assert_eq!(
            split_natural_paragraphs(
                vec![underfilled_previous.clone(), indented.clone()],
                LayoutLabel::Text,
            )
            .len(),
            2,
            "indent plus an underfilled previous line is a natural paragraph boundary"
        );

        let same_baseline = synthetic_text_line(&template, 30.0, 30.0, 100.0);
        assert_eq!(
            split_natural_paragraphs(vec![underfilled_previous, same_baseline], LayoutLabel::Text,)
                .len(),
            1,
            "same-baseline fragments are not consecutive visual lines"
        );

        let mut formula_fragment = synthetic_text_line(&template, 30.0, 30.0, 100.0);
        let layout = formula_fragment.chars[0].character.layout.as_mut().unwrap();
        layout.label = LayoutLabel::InlineFormula;
        layout.policy = TranslationPolicy::Passthrough;
        assert_eq!(
            split_natural_paragraphs(
                vec![
                    synthetic_text_line(&template, 10.0, 10.0, 100.0),
                    formula_fragment,
                ],
                LayoutLabel::Text,
            )
            .len(),
            2,
            "formula-bearing regions retain their established composition boundary"
        );

        let sparse_column = vec![
            synthetic_text_line(&template, 145.0, 10.0, 100.0),
            synthetic_text_line(&template, 140.0, 10.0, 88.0),
            synthetic_text_line(&template, 157.0, 10.0, 76.0),
        ];
        assert_eq!(
            split_natural_paragraphs(sparse_column, LayoutLabel::Text).len(),
            1,
            "the leftmost sparse-column outlier is not the normal line start"
        );

        assert_eq!(
            split_natural_paragraphs(
                vec![synthetic_text_line(&template, 10.0, 40.0, 100.0), indented,],
                LayoutLabel::Algorithm,
            )
            .len(),
            1,
            "passthrough categories do not create translation-request boundaries"
        );
    }

    #[test]
    fn first_line_indent_uses_baseline_origins_not_quote_ink() {
        let mut template = inspected_fixture_chars().remove(0);
        let layout = template.layout.as_mut().unwrap();
        layout.source = LayoutSource::Model;
        layout.label = LayoutLabel::Text;
        layout.policy = TranslationPolicy::Translate;
        let indented = vec![
            synthetic_text_line(&template, 30.0, 30.0, 100.0),
            synthetic_text_line(&template, 10.0, 60.0, 88.0),
            synthetic_text_line(&template, 10.0, 60.0, 76.0),
        ];
        assert_eq!(source_first_line_indent(&indented), Some(20.0));

        let mut quote = synthetic_text_line(&template, 10.0, 60.0, 100.0);
        quote.chars[0].character.unicode = Some('“');
        quote.chars[0].character.visual_bbox.left = 4.0;
        let prose = vec![quote, synthetic_text_line(&template, 10.0, 60.0, 88.0)];
        assert_eq!(source_first_line_indent(&prose), None);

        let mut footnote = indented.clone();
        for positioned in footnote.iter_mut().flat_map(|line| &mut line.chars) {
            positioned.character.layout.as_mut().unwrap().label = LayoutLabel::Footnote;
        }
        assert_eq!(source_first_line_indent(&footnote), None);

        let mut fallback = indented;
        for positioned in fallback.iter_mut().flat_map(|line| &mut line.chars) {
            positioned.character.layout.as_mut().unwrap().source = LayoutSource::FallbackLine;
        }
        assert_eq!(source_first_line_indent(&fallback), None);
    }

    #[test]
    fn body_font_size_mode_counts_characters_breaks_ties_up_and_excludes_scripts() {
        let template = inspected_fixture_chars().remove(0);
        let mut chars = (0..5).map(|_| template.clone()).collect::<Vec<_>>();
        for (character, size) in chars.iter_mut().zip([7.0, 7.0, 7.0, 10.0, 10.0]) {
            character.font_size = size;
        }
        let refs = chars.iter().collect::<Vec<_>>();
        assert_eq!(preferred_body_font_size(&refs), Some(7.0));
        assert!((preferred_typeset_font_size(&refs).unwrap() - 8.2).abs() <= f64::EPSILON);

        let tied = [&chars[0], &chars[1], &chars[3], &chars[4]];
        assert_eq!(preferred_body_font_size(&tied), Some(10.0));

        let mut anchor = template.clone();
        anchor.font_size = 10.0;
        anchor.baseline_origin = Point { x: 10.0, y: 100.0 };
        anchor.r#box = Rect {
            left: 10.0,
            bottom: 98.0,
            right: 20.0,
            top: 108.0,
        };
        let mut upper = anchor.clone();
        upper.font_size = 7.0;
        upper.baseline_origin = Point { x: 20.0, y: 104.0 };
        upper.r#box = Rect {
            left: 20.0,
            bottom: 102.0,
            right: 25.0,
            top: 109.0,
        };
        let mut lower = upper.clone();
        lower.baseline_origin = Point { x: 5.0, y: 96.0 };
        lower.r#box = Rect {
            left: 5.0,
            bottom: 94.0,
            right: 10.0,
            top: 101.0,
        };
        let script_chars = [&anchor, &upper, &lower];
        assert_eq!(preferred_body_font_size(&script_chars), Some(10.0));
        assert_eq!(preferred_typeset_font_size(&script_chars), Some(10.0));
    }

    #[test]
    fn typeset_preserves_absolute_first_line_indent_across_wrapping() {
        let mut chars = inspected_fixture_chars();
        chars.last_mut().unwrap().baseline_origin.y -= 12.0;
        let char_refs = chars.iter().collect::<Vec<_>>();
        let translated = "中文测"
            .chars()
            .map(|value| crate::translate::StyledCharacter { value, bold: false })
            .collect::<Vec<_>>();
        let planned = plan_text_segment(
            &char_refs,
            &translated,
            &BTreeSet::from([(9, 0)]),
            &test_output_fonts(),
            Rect {
                left: 0.0,
                bottom: 0.0,
                right: 300.0,
                top: 200.0,
            },
            &[],
            None,
            Some(20.0),
            None,
        )
        .unwrap();

        assert!(planned.baselines.len() >= 2);
        assert!((planned.baselines[0].0 - planned.baselines[1].0 - 20.0).abs() <= 0.001);
    }

    #[test]
    fn obstacle_slots_preserve_or_reject_the_absolute_first_line_indent() {
        let container = Rect {
            left: 10.0,
            bottom: 20.0,
            right: 100.0,
            top: 100.0,
        };
        let page = Rect {
            left: 0.0,
            bottom: 0.0,
            right: 120.0,
            top: 120.0,
        };
        let slots =
            obstacle_aware_multiline_slots(container, 88.0, 8.0, -2.0, 8.0, page, &[], Some(20.0));
        assert_eq!(slots[0].left, 30.0);
        assert!(
            slots
                .iter()
                .find(|slot| slot.baseline_y < 88.0 - 0.01)
                .is_some_and(|slot| slot.left == container.left),
            "continuation lines return to the source container edge"
        );

        let covering_indent = Rect {
            left: 25.0,
            bottom: 80.0,
            right: 40.0,
            top: 100.0,
        };
        assert!(
            obstacle_aware_multiline_slots(
                container,
                88.0,
                8.0,
                -2.0,
                8.0,
                page,
                &[covering_indent],
                Some(20.0),
            )
            .is_empty(),
            "an obstacle at the absolute indent must fail closed"
        );
    }

    fn scan_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-scan-01-image-only/unit-scan-01-image-only.pdf")
    }

    fn form_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../corpus/fixtures/unit-xobj-00-recursion-parent/unit-xobj-00-recursion-parent.pdf",
        )
    }

    fn singular_form_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-xobj-05-singular-ctm/unit-xobj-05-singular-ctm.pdf")
    }

    fn translate_fixture_once(
        translator: &dyn Translator,
        events: &RecordingEventSink,
        config: crate::context::PipelineConfig,
    ) -> Result<Document> {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator,
            events,
            snapshots: None,
            config,
        };
        inspect(&mut document, &context)?;
        styles_and_formulas(&mut document, &context)?;
        extract_terms(&mut document, &context)?;
        translate(&mut document, &context)?;
        Ok(document)
    }

    fn nested_text_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/mal-stream-nested-bt/mal-stream-nested-bt.pdf")
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
    fn recovered_multi_region_page_remains_processable_after_paragraph_grouping() {
        let mut document = Document::for_inspection(nested_text_fixture());
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

        assert_eq!(document.extracted_pages[0].layout_regions.len(), 2);
        assert_eq!(document.il.pages[0].paragraphs.len(), 2);
        assert_eq!(
            document.il.pages[0]
                .paragraphs
                .iter()
                .map(Paragraph::source_text)
                .collect::<String>(),
            "MIMUS"
        );
        assert!(
            document.extracted_pages[0]
                .recoveries
                .contains(&RecoveryKind::NestedTextObject)
        );
    }

    #[test]
    fn recorded_fallback_inside_table_stays_passthrough_through_translation() {
        let recording = br#"{
            "schema_version": 1,
            "pages": [{
                "page_index": 0,
                "geometry": {"width": 300.0, "height": 200.0, "rotate_degrees": 0},
                "regions": [
                    {
                        "bounds": {"left": 0.0, "bottom": 100.0, "right": 300.0, "top": 150.0},
                        "reading_order": 0,
                        "label": "table",
                        "source": "model",
                        "confidence": 0.99
                    },
                    {
                        "bounds": {"left": 70.0, "bottom": 118.0, "right": 112.0, "top": 132.0},
                        "reading_order": 1,
                        "label": "fallback_line",
                        "source": "fallback_line",
                        "confidence": 1.0
                    }
                ]
            }]
        }"#;
        let detector = crate::engine::RecordedLayoutDetector::from_bytes(recording).unwrap();
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = WrappingTranslator::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &detector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };

        inspect(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();

        let paragraph = &document.il.pages[0].paragraphs[0];
        assert_eq!(paragraph.translated_text.as_deref(), Some("MIMUS"));
        assert!(translator.inputs.lock().unwrap().is_empty());
        let assignments = paragraph
            .chars()
            .iter()
            .map(|character| character.layout)
            .collect::<Vec<_>>();
        assert!(
            paragraph.chars().iter().all(|character| {
                character.layout.is_some_and(|layout| {
                    layout.label == LayoutLabel::Table
                        && layout.source == LayoutSource::Model
                        && layout.reading_order == 0
                        && layout.policy == TranslationPolicy::Passthrough
                })
            }),
            "{assignments:#?}"
        );
    }

    #[test]
    fn positional_policy_repairs_model_regions_without_relabeling_fallback_lines() {
        let bounds = Rect {
            left: 20.0,
            bottom: 180.0,
            right: 180.0,
            top: 195.0,
        };
        let mut regions = [
            crate::engine::LayoutRegion {
                bounds,
                reading_order: 0,
                label: LayoutLabel::Text,
                source: LayoutSource::Model,
                confidence: 0.8,
            },
            crate::engine::LayoutRegion {
                bounds,
                reading_order: 1,
                label: LayoutLabel::FallbackLine,
                source: LayoutSource::FallbackLine,
                confidence: 1.0,
            },
        ];

        apply_policy_overrides(
            PageGeometry {
                width: 200.0,
                height: 200.0,
                rotate_degrees: 0,
            },
            &mut regions,
            &[],
        );

        assert_eq!(regions[0].label, LayoutLabel::Header);
        assert_eq!(regions[1].label, LayoutLabel::FallbackLine);
    }

    #[test]
    fn positional_policy_keeps_page_top_titles_translatable() {
        let mut regions = [crate::engine::LayoutRegion {
            bounds: Rect {
                left: 20.0,
                bottom: 180.0,
                right: 180.0,
                top: 195.0,
            },
            reading_order: 0,
            label: LayoutLabel::ParagraphTitle,
            source: LayoutSource::Model,
            confidence: 0.9,
        }];

        apply_policy_overrides(
            PageGeometry {
                width: 200.0,
                height: 200.0,
                rotate_degrees: 0,
            },
            &mut regions,
            &[],
        );

        assert_eq!(regions[0].label, LayoutLabel::ParagraphTitle);
        assert_eq!(
            regions[0].label.translation_policy(),
            TranslationPolicy::Translate
        );
    }

    #[test]
    fn positional_policy_only_relabels_short_generic_text_apparatus() {
        assert!(is_positional_apparatus(LayoutLabel::Text, "Short header"));
        assert!(!is_positional_apparatus(
            LayoutLabel::Text,
            &"long running prose ".repeat(8)
        ));
        for label in [
            LayoutLabel::Abstract,
            LayoutLabel::DocTitle,
            LayoutLabel::FigureTitle,
            LayoutLabel::Footnote,
            LayoutLabel::ParagraphTitle,
        ] {
            assert!(!is_positional_apparatus(label, "Short semantic region"));
        }
    }

    #[test]
    fn numbered_caption_shape_requires_a_prefix_number_and_colon() {
        assert!(looks_like_numbered_caption("Table1: Results"));
        assert!(looks_like_numbered_caption("Table 4: Results"));
        assert!(looks_like_numbered_caption("Figure 2: Architecture"));
        assert!(!looks_like_numbered_caption("Table 3 row (E)"));
        assert!(!looks_like_numbered_caption("To evaluate Table 3"));
    }

    #[test]
    fn arabic_section_number_does_not_consume_a_title_leading_roman_letter() {
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
        let source = document.il.pages[0].paragraphs[0].chars();
        let assignment = LayoutAssignment {
            label: LayoutLabel::ParagraphTitle,
            reading_order: 0,
            bounds: Rect {
                left: 70.0,
                bottom: 110.0,
                right: 110.0,
                top: 135.0,
            },
            source: LayoutSource::Model,
            policy: TranslationPolicy::Translate,
        };
        let mut positioned = source[..3]
            .iter()
            .cloned()
            .enumerate()
            .map(|(walked_index, mut character)| {
                character.layout = Some(assignment);
                PositionedChar {
                    walked_index,
                    locatable: true,
                    character,
                    force_no_space_before: false,
                    small_edge_character: false,
                }
            })
            .collect::<Vec<_>>();
        positioned[0].character.unicode = Some('1');
        positioned[0].character.r#box.left = 72.0;
        positioned[0].character.r#box.right = 78.0;
        positioned[0].character.baseline_origin.x = 72.0;
        positioned[1].character.unicode = Some('I');
        positioned[1].character.r#box.left = 90.0;
        positioned[1].character.r#box.right = 94.0;
        positioned[1].character.baseline_origin.x = 90.0;
        positioned[2].character.unicode = Some('n');
        positioned[2].character.r#box.left = 94.0;
        positioned[2].character.r#box.right = 100.0;
        positioned[2].character.baseline_origin.x = 94.0;

        mark_leading_section_number(&mut positioned);

        assert_eq!(
            positioned[0].character.layout.unwrap().label,
            LayoutLabel::Number
        );
        assert_eq!(
            positioned[0].character.layout.unwrap().policy,
            TranslationPolicy::Passthrough
        );
        assert!(positioned[1..].iter().all(|positioned| {
            positioned.character.layout.is_some_and(|layout| {
                layout.label == LayoutLabel::ParagraphTitle
                    && layout.policy == TranslationPolicy::Translate
            })
        }));
        assert!(positioned[1].force_no_space_before);
        let lines = build_text_lines(positioned);
        assert_eq!(
            lines.len(),
            1,
            "a retained section number and its title must remain one visual heading line"
        );
    }

    #[test]
    fn model_ownership_requires_matching_fallback_line_semantics() {
        let bounds = Rect {
            left: 8.0,
            bottom: 10.0,
            right: 9.0,
            top: 12.0,
        };
        let model = crate::engine::LayoutRegion {
            bounds: Rect {
                left: 5.0,
                bottom: 0.0,
                right: 20.0,
                top: 20.0,
            },
            reading_order: 3,
            label: LayoutLabel::Table,
            source: LayoutSource::Model,
            confidence: 0.9,
        };
        let mut fallback = crate::engine::LayoutRegion {
            bounds: Rect {
                left: 0.0,
                bottom: 9.0,
                right: 10.0,
                top: 13.0,
            },
            reading_order: 4,
            label: LayoutLabel::Text,
            source: LayoutSource::FallbackLine,
            confidence: 1.0,
        };

        let assignment = layout_assignment(&[model, fallback], bounds, None, false).unwrap();
        assert_eq!(assignment.label, LayoutLabel::Text);
        assert_eq!(assignment.source, LayoutSource::FallbackLine);

        fallback.label = LayoutLabel::Table;
        let assignment = layout_assignment(&[model, fallback], bounds, None, false).unwrap();
        assert_eq!(assignment.label, LayoutLabel::Table);
        assert_eq!(assignment.source, LayoutSource::Model);
        assert_eq!(assignment.reading_order, 3);
        assert_eq!(assignment.policy, TranslationPolicy::Passthrough);

        let assignment = layout_assignment(&[model, fallback], bounds, None, true).unwrap();
        assert_eq!(assignment.label, LayoutLabel::Table);
        assert_eq!(assignment.policy, TranslationPolicy::Translate);
    }

    #[test]
    fn small_edge_ownership_requires_one_overlapping_model_and_tiny_ink() {
        let bounds = Rect {
            left: 8.0,
            bottom: 10.0,
            right: 10.0,
            top: 12.0,
        };
        let tiny_visual = Rect {
            left: 9.0,
            bottom: 10.5,
            right: 9.1,
            top: 10.6,
        };
        let ordinary_visual = Rect {
            left: 8.0,
            bottom: 10.0,
            right: 10.0,
            top: 12.0,
        };
        let model = crate::engine::LayoutRegion {
            bounds: Rect {
                left: 0.0,
                bottom: 0.0,
                right: 8.75,
                top: 20.0,
            },
            reading_order: 3,
            label: LayoutLabel::Table,
            source: LayoutSource::Model,
            confidence: 0.9,
        };
        let fallback = crate::engine::LayoutRegion {
            bounds,
            reading_order: 4,
            label: LayoutLabel::Text,
            source: LayoutSource::FallbackLine,
            confidence: 1.0,
        };

        let small_owner =
            small_edge_model_owner(&[model, fallback], bounds, tiny_visual, Some(50.0));
        assert_eq!(small_owner, Some(0));
        let assignment = layout_assignment(&[model, fallback], bounds, small_owner, false).unwrap();
        assert_eq!(assignment.source, LayoutSource::Model);

        let ordinary_owner =
            small_edge_model_owner(&[model, fallback], bounds, ordinary_visual, Some(50.0));
        assert_eq!(ordinary_owner, None);
        let assignment =
            layout_assignment(&[model, fallback], bounds, ordinary_owner, false).unwrap();
        assert_eq!(assignment.source, LayoutSource::FallbackLine);

        let second_model = crate::engine::LayoutRegion {
            bounds: Rect {
                left: 8.5,
                bottom: 0.0,
                right: 20.0,
                top: 20.0,
            },
            reading_order: 5,
            ..model
        };
        let ambiguous_owner = small_edge_model_owner(
            &[model, second_model, fallback],
            bounds,
            tiny_visual,
            Some(50.0),
        );
        assert_eq!(ambiguous_owner, None);
        let assignment = layout_assignment(
            &[model, second_model, fallback],
            bounds,
            ambiguous_owner,
            false,
        )
        .unwrap();
        assert_eq!(assignment.source, LayoutSource::FallbackLine);
    }

    #[test]
    fn paragraph_order_prefers_model_order_over_geometry() {
        let recording = br#"{
            "schema_version": 1,
            "pages": [{
                "page_index": 0,
                "geometry": {"width": 300.0, "height": 200.0, "rotate_degrees": 0},
                "regions": [
                    {
                        "bounds": {"left": 71.0, "bottom": 115.0, "right": 96.251, "top": 133.0},
                        "reading_order": 1,
                        "label": "text",
                        "source": "model",
                        "confidence": 0.99
                    },
                    {
                        "bounds": {"left": 96.252, "bottom": 115.0, "right": 114.0, "top": 133.0},
                        "reading_order": 0,
                        "label": "text",
                        "source": "model",
                        "confidence": 0.99
                    }
                ]
            }]
        }"#;
        let detector = crate::engine::RecordedLayoutDetector::from_bytes(recording).unwrap();
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = CountingTranslator::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &detector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };

        inspect(&mut document, &context).unwrap();

        assert_eq!(
            document.il.pages[0]
                .paragraphs
                .iter()
                .map(Paragraph::source_text)
                .collect::<Vec<_>>(),
            ["US".to_owned(), "MIM".to_owned()]
        );
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
    fn unsupported_translated_glyphs_preserve_the_paragraph() {
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
            config: config_with_test_output_fonts(),
        };

        run(&mut document, &context).unwrap();
        assert_eq!(
            std::fs::read(&output).unwrap(),
            std::fs::read(fixture()).unwrap()
        );
        assert!(document.rewrites.is_empty());
        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::UnsupportedFont)
        );
    }

    #[test]
    fn translated_cjk_builds_a_deterministic_searchable_subset() {
        let mut document = Document::new(fixture(), "unused.pdf");
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CjkTranslator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        for pass in [parse as Pass, scan_detect, layout, paragraph_find] {
            pass(&mut document, &context).unwrap();
        }
        for character in match &mut document.il.pages[0].paragraphs[0].text {
            TextCarrier::Chars { chars } => chars,
        } {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 90.0,
                right: 122.0,
                top: 135.0,
            };
        }
        document.extracted_pages[0].layout_regions[0].bounds = Rect {
            left: 72.0,
            bottom: 90.0,
            right: 122.0,
            top: 135.0,
        };
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();
        font_embed(&mut document, &context).unwrap();

        let rewrite = &document.rewrites[0];
        let [publication] = document.il.publication_ink.as_slice() else {
            panic!("the translated paragraph must expose final publication ink");
        };
        assert_eq!((publication.page_index, publication.reading_order), (0, 0));
        assert_eq!(
            publication.crop_box,
            document.extracted_pages[0].frame.unwrap().crop_box
        );
        assert_eq!(publication.admissible_container.left, 72.0);
        assert_eq!(publication.admissible_container.right, 122.0);
        assert!(publication.components.iter().all(|component| {
            rect_contains(publication.admissible_container, component.bounds(), 0.01)
        }));
        let publication_glyphs = publication
            .components
            .iter()
            .filter_map(|component| match component {
                il::PublicationInkComponent::TranslatedText {
                    ownership_group,
                    glyphs,
                    ..
                } => {
                    assert_eq!(*ownership_group, 0);
                    Some(glyphs)
                }
                _ => None,
            })
            .flatten()
            .map(|glyph| (glyph.unicode, glyph.baseline_origin))
            .collect::<Vec<_>>();
        assert_eq!(
            publication_glyphs,
            rewrite
                .typeset_characters
                .iter()
                .map(|glyph| (glyph.unicode, glyph.baseline_origin))
                .collect::<Vec<_>>()
        );
        let font = &rewrite.embedded_fonts[0];
        assert_eq!(
            font.glyphs
                .iter()
                .map(|(_, value, _)| *value)
                .collect::<BTreeSet<_>>(),
            "MIMUS中文测试".chars().collect()
        );
        assert!(font.base_font.ends_with("+MimusTestGB2312Regular-Regular"));
        let original = document.pdf.as_ref().unwrap();
        let (first, _) =
            build_incremental(&document.original_bytes, original, &document.rewrites).unwrap();
        let (second, _) =
            build_incremental(&document.original_bytes, original, &document.rewrites).unwrap();
        assert_eq!(first, second);
        if let Ok(path) = std::env::var("MIMUS_TEST_OUTPUT") {
            std::fs::write(path, &first).unwrap();
        }
        let output = LopdfDocument::load_mem(&first).unwrap();
        let page_id = output.get_pages()[&1];
        let fonts = output.get_page_fonts(page_id).unwrap();
        let type0 = fonts.get(b"MimusR".as_slice()).unwrap();
        let cmap_id = type0.get(b"ToUnicode").unwrap().as_reference().unwrap();
        let cmap = output
            .get_object(cmap_id)
            .unwrap()
            .as_stream()
            .unwrap()
            .decompressed_content()
            .unwrap();
        let cmap = String::from_utf8(cmap).unwrap();
        for (cid, value, _) in &font.glyphs {
            assert!(cmap.contains(&format!("<{cid:04X}> <{:04X}>", u32::from(*value))));
        }
        let pdfium = crate::engine::pdfium::PdfiumEngine::from_environment().unwrap();
        let actual = pdfium.page_characters(&first, 0).unwrap();
        validate_typeset_characters(0, &rewrite.typeset_characters, &actual, 0.01).unwrap();
        let mut reversed = actual.clone();
        reversed.reverse();
        validate_typeset_characters(0, &rewrite.typeset_characters, &reversed, 0.01).unwrap();
        pdfium
            .rasterize_page(&first, 0)
            .unwrap()
            .validate()
            .unwrap();
        let mut baselines = rewrite
            .typeset_characters
            .iter()
            .map(|character| character.baseline_origin.y)
            .collect::<Vec<_>>();
        baselines.sort_by(f64::total_cmp);
        baselines.dedup_by(|left, right| (*left - *right).abs() < 0.001);
        assert_eq!(baselines.len(), 2);
        assert!(baselines[1] - baselines[0] >= MIN_FONT_SIZE_PT * LINE_ADVANCE_EM);
        let bold_source = &context.config.output_fonts.as_ref().unwrap().bold;
        let (bold, bold_cids) = build_embedded_font(
            &"MIMUS中文测试".chars().collect(),
            bold_source,
            OutputFontKey::PrimaryBold,
        )
        .unwrap();
        assert!(bold.base_font.ends_with("+MimusTestGB2312Bold-Bold"));
        assert_eq!(bold_cids.len(), font.glyphs.len());
        assert_ne!(bold.font_bytes, font.font_bytes);
    }

    #[test]
    fn variable_output_font_bold_instance_configures_metrics_and_subsets() {
        let output_fonts = test_variable_output_fonts();
        let regular_instance =
            output_font_instance(&output_fonts.regular.bytes, OutputFontKey::PrimaryRegular)
                .unwrap();
        let bold_instance =
            output_font_instance(&output_fonts.bold.bytes, OutputFontKey::PrimaryBold).unwrap();
        assert_eq!(
            regular_instance.variations,
            [OutputFontVariation {
                tag: *b"wght",
                value: 400.0,
            }]
        );
        assert_eq!(
            bold_instance.variations,
            [OutputFontVariation {
                tag: *b"wght",
                value: 700.0,
            }]
        );
        assert_eq!(
            regular_instance.postscript_name.as_deref(),
            Some("NotoSansSC-Regular")
        );
        assert_eq!(
            bold_instance.postscript_name.as_deref(),
            Some("NotoSansSC-Bold")
        );

        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let regular_face = faces.face(OutputFontKey::PrimaryRegular);
        let bold_face = faces.face(OutputFontKey::PrimaryBold);
        let regular_glyph = regular_face.glyph_index('M').unwrap();
        let bold_glyph = bold_face.glyph_index('M').unwrap();
        let default_face = ttf_parser::Face::parse(&output_fonts.regular.bytes, 0).unwrap();
        assert_ne!(
            regular_face.glyph_hor_advance(regular_glyph),
            default_face.glyph_hor_advance(default_face.glyph_index('M').unwrap())
        );
        let regular_han = regular_face.glyph_index('中').unwrap();
        let default_han = default_face.glyph_index('中').unwrap();
        assert_ne!(
            regular_face.glyph_bounding_box(regular_han),
            default_face.glyph_bounding_box(default_han)
        );
        assert_eq!(bold_face.glyph_hor_advance(bold_glyph), Some(853));
        assert_ne!(
            regular_face.glyph_bounding_box(regular_glyph),
            bold_face.glyph_bounding_box(bold_glyph)
        );

        let used = BTreeSet::from(['M', '中', '文', '测', '试']);
        let (regular, regular_cids) =
            build_embedded_font(&used, &output_fonts.regular, OutputFontKey::PrimaryRegular)
                .unwrap();
        let (bold, bold_cids) =
            build_embedded_font(&used, &output_fonts.bold, OutputFontKey::PrimaryBold).unwrap();
        assert_ne!(regular.font_bytes, bold.font_bytes);

        assert!(regular.base_font.ends_with("+NotoSansSC-Regular"));
        assert!(bold.base_font.ends_with("+NotoSansSC-Bold"));

        for (embedded, cids, source) in [
            (&regular, &regular_cids, regular_face),
            (&bold, &bold_cids, bold_face),
        ] {
            let subset_face = ttf_parser::Face::parse(&embedded.font_bytes, 0).unwrap();
            for (cid, value, embedded_advance) in &embedded.glyphs {
                assert_eq!(cids[value], *cid);
                let source_glyph = source.glyph_index(*value).unwrap();
                assert_eq!(
                    source.glyph_hor_advance(source_glyph),
                    Some(*embedded_advance)
                );
                assert_eq!(
                    subset_face.glyph_hor_advance(ttf_parser::GlyphId(*cid)),
                    Some(*embedded_advance)
                );
                let actual_bounds = subset_face
                    .glyph_bounding_box(ttf_parser::GlyphId(*cid))
                    .unwrap();
                let expected_bounds = source.glyph_bounding_box(source_glyph).unwrap();
                for (actual, expected) in [
                    (actual_bounds.x_min, expected_bounds.x_min),
                    (actual_bounds.y_min, expected_bounds.y_min),
                    (actual_bounds.x_max, expected_bounds.x_max),
                    (actual_bounds.y_max, expected_bounds.y_max),
                ] {
                    assert!(
                        actual.abs_diff(expected) <= 1,
                        "{value} subset outline bound {actual} differs from configured source {expected}"
                    );
                }
                assert_eq!(
                    glyph_width_1000(*embedded_advance, embedded.units_per_em),
                    (glyph_advance_em(source, source_glyph).unwrap() * 1000.0) as u32
                );
            }
        }
    }

    #[test]
    #[ignore = "requires the SHA-pinned production variable font"]
    fn pinned_full_variable_font_regular_subset_matches_wght_400_outline() {
        use sha2::{Digest, Sha256};

        let path = std::env::var("MIMUS_PINNED_OUTPUT_FONT")
            .expect("MIMUS_PINNED_OUTPUT_FONT must point to the pinned production VF");
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "69467baf421bdbb32b292d6c092ed033ca32e5f7a0d06194e69901287b50b2f3"
        );
        let source = crate::context::OutputFont {
            bytes,
            postscript_name: "NotoSerifSC-ExtraLight".to_owned(),
            source: "test:pinned-production-vf".to_owned(),
            sha256: "69467baf421bdbb32b292d6c092ed033ca32e5f7a0d06194e69901287b50b2f3".to_owned(),
        };

        let default_face = ttf_parser::Face::parse(&source.bytes, 0).unwrap();
        let regular_face =
            configured_output_font_face(&source.bytes, OutputFontKey::PrimaryRegular).unwrap();
        let default_bounds = default_face
            .glyph_bounding_box(default_face.glyph_index('一').unwrap())
            .unwrap();
        let regular_glyph = regular_face.glyph_index('一').unwrap();
        let regular_bounds = regular_face.glyph_bounding_box(regular_glyph).unwrap();
        assert_ne!(default_bounds, regular_bounds);

        let (embedded, cids) = build_embedded_font(
            &BTreeSet::from(['一']),
            &source,
            OutputFontKey::PrimaryRegular,
        )
        .unwrap();
        assert!(embedded.base_font.ends_with("+NotoSerifSC-Regular"));
        let subset_face = ttf_parser::Face::parse(&embedded.font_bytes, 0).unwrap();
        let subset_bounds = subset_face
            .glyph_bounding_box(ttf_parser::GlyphId(cids[&'一']))
            .unwrap();
        for (actual, expected) in [
            (subset_bounds.x_min, regular_bounds.x_min),
            (subset_bounds.y_min, regular_bounds.y_min),
            (subset_bounds.x_max, regular_bounds.x_max),
            (subset_bounds.y_max, regular_bounds.y_max),
        ] {
            assert!(actual.abs_diff(expected) <= 1);
        }
        println!(
            "U+4E00 bounds: default={default_bounds:?}, wght400={regular_bounds:?}, subset={subset_bounds:?}"
        );
    }

    #[test]
    fn static_output_font_subsets_remain_byte_compatible() {
        let output_fonts = test_output_fonts();
        assert!(
            output_font_instance(&output_fonts.regular.bytes, OutputFontKey::PrimaryRegular)
                .unwrap()
                .variations
                .is_empty()
        );
        let used = BTreeSet::from(['M', '中']);
        let (embedded, _) =
            build_embedded_font(&used, &output_fonts.regular, OutputFontKey::PrimaryRegular)
                .unwrap();
        let face = ttf_parser::Face::parse(&output_fonts.regular.bytes, 0).unwrap();
        let mut remapper = subsetter::GlyphRemapper::new();
        for value in &used {
            remapper.remap(face.glyph_index(*value).unwrap().0);
        }
        let legacy = subsetter::subset(&output_fonts.regular.bytes, 0, &remapper).unwrap();
        assert_eq!(embedded.font_bytes, legacy);
    }

    #[test]
    fn variable_font_geometry_checks_use_configured_8pt_ink() {
        let output_fonts = test_variable_output_fonts();
        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let line = |bold| [crate::translate::StyledCharacter { value: 'M', bold }];
        let regular =
            styled_line_ink_bounds(&line(false), &faces, MIN_FONT_SIZE_PT, 0.0, 0.0).unwrap();
        let bold = styled_line_ink_bounds(&line(true), &faces, MIN_FONT_SIZE_PT, 0.0, 0.0).unwrap();
        assert!(bold.right > regular.right);

        let boundary = (regular.right + bold.right) / 2.0;
        let crop_box = Rect {
            left: -1.0,
            bottom: regular.bottom.min(bold.bottom) - 1.0,
            right: boundary,
            top: regular.top.max(bold.top) + 1.0,
        };
        assert!(ink_bounds_are_safe(&[regular], crop_box, &[]));
        assert!(!ink_bounds_are_safe(&[bold], crop_box, &[]));

        let page = Rect {
            right: bold.right + 1.0,
            ..crop_box
        };
        let obstacle = Rect {
            left: boundary,
            bottom: bold.bottom,
            right: bold.right + 0.1,
            top: bold.top,
        };
        assert!(ink_bounds_are_safe(&[regular], page, &[obstacle]));
        assert!(!ink_bounds_are_safe(&[bold], page, &[obstacle]));
    }

    #[test]
    fn variable_font_pdf_widths_and_extractor_positions_match_planning() {
        let output_fonts = test_variable_output_fonts();
        let config = crate::context::PipelineConfig {
            output_fonts: Some(output_fonts),
            ..crate::context::PipelineConfig::default()
        };
        let translator = VariableFontTranslator;
        let events = RecordingEventSink::default();
        let mut document = translate_fixture_once(&translator, &events, config.clone()).unwrap();
        for character in match &mut document.il.pages[0].paragraphs[0].text {
            TextCarrier::Chars { chars } => chars,
        } {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 80.0,
                right: 150.0,
                top: 140.0,
            };
        }
        document.extracted_pages[0].layout_regions[0].bounds = Rect {
            left: 72.0,
            bottom: 80.0,
            right: 150.0,
            top: 140.0,
        };
        let engine = FakeEngine::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config,
        };

        typeset(&mut document, &context).unwrap();
        font_embed(&mut document, &context).unwrap();
        let rewrite = &document.rewrites[0];
        let embedded = rewrite
            .embedded_fonts
            .iter()
            .find(|font| font.resource_name == OutputFontKey::PrimaryRegular.resource_name())
            .unwrap();
        let original = document.pdf.as_ref().unwrap();
        let (bytes, _) =
            build_incremental(&document.original_bytes, original, &document.rewrites).unwrap();
        let output = LopdfDocument::load_mem(&bytes).unwrap();
        let page_id = output.get_pages()[&1];
        let fonts = output.get_page_fonts(page_id).unwrap();
        let type0 = fonts.get(b"MimusR".as_slice()).unwrap();
        let descendant_id = type0.get(b"DescendantFonts").unwrap().as_array().unwrap()[0]
            .as_reference()
            .unwrap();
        let widths = output
            .get_dictionary(descendant_id)
            .unwrap()
            .get(b"W")
            .unwrap()
            .as_array()
            .unwrap();
        let published_widths = widths
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                (
                    u16::try_from(pair[0].as_i64().unwrap()).unwrap(),
                    u32::try_from(pair[1].as_array().unwrap()[0].as_i64().unwrap()).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for (cid, _, advance) in &embedded.glyphs {
            assert_eq!(
                published_widths[cid],
                glyph_width_1000(*advance, embedded.units_per_em)
            );
        }

        let pdfium = crate::engine::pdfium::PdfiumEngine::from_environment().unwrap();
        let extracted = pdfium.page_characters(&bytes, 0).unwrap();
        validate_typeset_characters(0, &rewrite.typeset_characters, &extracted, 0.01).unwrap();
    }

    #[test]
    fn translated_text_requires_output_fonts_from_the_pass_context() {
        let mut document = Document::new(fixture(), "unused.pdf");
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CjkTranslator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        for pass in [
            parse as Pass,
            scan_detect,
            layout,
            paragraph_find,
            translate,
        ] {
            pass(&mut document, &context).unwrap();
        }

        let error = typeset(&mut document, &context).unwrap_err();

        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Asset(crate::error::AssetReason::OutputFontUnavailable)
        );
    }

    #[test]
    fn missing_output_glyphs_preserve_only_the_paragraph_and_identify_the_font() {
        let mut document = Document::new(fixture(), "unused.pdf");
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let font_bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../corpus/fonts/MimusExact.ttf"
        ));
        let output_font = crate::context::OutputFont {
            bytes: font_bytes.to_vec(),
            postscript_name: "MimusExact".to_owned(),
            source: "test:missing-glyph".to_owned(),
            sha256: "6e1e40974dce5dca579f3f191dd7dcc9953e6e04165d69f36d01aa8242a24735".to_owned(),
        };
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CjkTranslator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                output_fonts: Some(crate::context::OutputFonts {
                    regular: output_font.clone(),
                    bold: output_font.clone(),
                    fallback_regular: output_font.clone(),
                    fallback_bold: output_font,
                }),
                ..crate::context::PipelineConfig::default()
            },
        };
        for pass in [
            parse as Pass,
            scan_detect,
            layout,
            paragraph_find,
            translate,
        ] {
            pass(&mut document, &context).unwrap();
        }

        typeset(&mut document, &context).unwrap();

        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::UnsupportedFont)
        );
        assert!(document.diagnostics.entries().iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::UnsupportedOutputGlyph {
                page_index: 0,
                reading_order: 0,
                missing_characters,
                font_source,
                font_sha256,
                fallback_font_source,
                fallback_font_sha256,
            } if missing_characters == "中文测试"
                && font_source == "test:missing-glyph"
                && font_sha256 == "6e1e40974dce5dca579f3f191dd7dcc9953e6e04165d69f36d01aa8242a24735"
                && fallback_font_source == "test:missing-glyph"
                && fallback_font_sha256 == "6e1e40974dce5dca579f3f191dd7dcc9953e6e04165d69f36d01aa8242a24735"
        )));
    }

    #[test]
    fn translated_line_breaks_are_normalized_before_font_coverage() {
        let styled = "中\n\n文\t"
            .chars()
            .map(|value| crate::translate::StyledCharacter { value, bold: false })
            .collect::<Vec<_>>();
        assert_eq!(
            normalize_typeset_whitespace(&styled)
                .iter()
                .map(|character| character.value)
                .collect::<String>(),
            "中 文"
        );
        let translator = StaticTranslator {
            output: "中\n\n\t",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let mut document =
            translate_fixture_once(&translator, &events, config_with_test_output_fonts()).unwrap();
        for character in match &mut document.il.pages[0].paragraphs[0].text {
            TextCarrier::Chars { chars } => chars,
        } {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 90.0,
                right: 122.0,
                top: 135.0,
            };
        }
        document.extracted_pages[0].layout_regions[0].bounds = Rect {
            left: 72.0,
            bottom: 90.0,
            right: 122.0,
            top: 135.0,
        };
        let engine = FakeEngine::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };

        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        let output = document.rewrites[0]
            .typeset_characters
            .iter()
            .map(|character| character.unicode)
            .collect::<String>();
        assert_eq!(output, "中");
    }

    #[test]
    fn single_line_ink_expansion_is_bounded_by_obstacles_and_page() {
        let output_fonts = test_output_fonts();
        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let line = "中文"
            .chars()
            .map(|value| crate::translate::StyledCharacter { value, bold: false })
            .collect::<Vec<_>>();
        let container = Rect {
            left: 72.0,
            bottom: 119.0,
            right: 122.0,
            top: 129.0,
        };
        let page = Rect {
            left: 0.0,
            bottom: 0.0,
            right: 300.0,
            top: 200.0,
        };
        let expansion = single_line_ink_fit(
            &line,
            &faces,
            12.0,
            container.left,
            120.0,
            container,
            page,
            &[],
            None,
        )
        .unwrap()
        .expansion
        .unwrap();
        assert!(expansion.top_pt <= SINGLE_LINE_MAX_VERTICAL_OVERFLOW_PT);
        assert!(expansion.bottom_pt <= SINGLE_LINE_MAX_VERTICAL_OVERFLOW_PT);

        let ink = styled_line_ink_bounds(&line, &faces, 12.0, container.left, 120.0).unwrap();
        assert!(
            single_line_ink_fit(
                &line,
                &faces,
                12.0,
                container.left,
                120.0,
                container,
                page,
                &[ink],
                None,
            )
            .is_none()
        );
        let prefix_obstacle = Rect {
            left: container.left,
            bottom: ink.bottom,
            right: container.left + 12.0,
            top: ink.top,
        };
        assert!(
            single_line_ink_fit(
                &line,
                &faces,
                12.0,
                container.left,
                120.0,
                container,
                page,
                &[prefix_obstacle],
                None,
            )
            .is_none()
        );
        assert!(
            single_line_ink_fit(
                &line,
                &faces,
                12.0,
                container.left + 14.0,
                120.0,
                container,
                page,
                &[prefix_obstacle],
                None,
            )
            .is_some()
        );
        assert!(
            single_line_ink_fit(
                &line,
                &faces,
                12.0,
                container.left,
                120.0,
                container,
                Rect {
                    top: ink.top - 0.1,
                    ..page
                },
                &[],
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn bold_single_line_ink_rebases_inside_container_before_expanding() {
        let output_fonts = test_variable_output_fonts();
        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let line = [crate::translate::StyledCharacter {
            value: 'M',
            bold: true,
        }];
        let baseline_y = 120.0;
        let ink = styled_line_ink_bounds(&line, &faces, 12.0, 72.0, baseline_y).unwrap();
        let container = Rect {
            left: 71.0,
            bottom: ink.bottom - 1.0,
            right: ink.right + 1.0,
            top: ink.top - 0.25,
        };
        let page = Rect {
            left: 0.0,
            bottom: 0.0,
            right: 300.0,
            top: 200.0,
        };

        let fit = single_line_ink_fit(
            &line,
            &faces,
            12.0,
            72.0,
            baseline_y,
            container,
            page,
            &[],
            None,
        )
        .unwrap();
        assert!(fit.baseline_y < baseline_y);
        assert!(fit.expansion.is_none());
        assert!(rect_contains(container, fit.ink_bounds, 0.01));
    }

    /// 真实论文 `1706.03762v7.pdf` 页 12（0 基）段 69 `Attention Visualizations` 的
    /// 几何：容器与原文足迹取自 L5-4 的 typeset IL，障碍取自被外层 Form `/BBox`
    /// 裁掉的 `Input-Input Layer5` 的 `visual_bbox`（#110）。
    #[test]
    fn form_clipped_phantom_ink_is_the_only_thing_blocking_the_heading_it_covers() {
        let output_fonts = test_output_fonts();
        let container = Rect {
            left: 108.0,
            bottom: 707.5383632,
            right: 230.767_948_8,
            top: 718.286_088,
        };
        let page_bounds = Rect {
            left: 0.0,
            bottom: 0.0,
            right: 612.0,
            top: 792.0,
        };
        // `Input-Input Layer5` 的 18 个 glyph 里与容器相交的那 16 个的包络。
        let phantom = Rect {
            left: 109.110_816_955_566_4,
            bottom: 711.335_205_078_125,
            right: 234.453_857_421_875,
            top: 726.445_617_675_781_2,
        };
        let source = "Attention Visualizations"
            .chars()
            .enumerate()
            .map(|(index, unicode)| Char {
                unicode: Some(unicode),
                unicode_source: None,
                code: unicode as u32,
                visible: true,
                font: crate::il::FontRef {
                    resource_name: "F87".to_owned(),
                    object_number: 105,
                    generation: 0,
                },
                font_size: 11.9552,
                baseline_origin: crate::il::Point {
                    x: 108.0 + index as f64 * 5.0,
                    y: 710.037,
                },
                r#box: container,
                visual_bbox: container,
                bbox_estimated: false,
                text_transform: TextTransform::Upright,
                implicit_space_before: false,
                layout: Some(LayoutAssignment {
                    label: LayoutLabel::FallbackLine,
                    reading_order: 288,
                    bounds: container,
                    source: LayoutSource::FallbackLine,
                    policy: TranslationPolicy::Translate,
                }),
                passthrough: PassthroughRef {
                    content_object: 356,
                    byte_start: 50,
                    byte_end: 87,
                    encoded: vec![unicode as u8],
                },
            })
            .collect::<Vec<_>>();
        let chars = source.iter().collect::<Vec<_>>();
        let translated = "中文"
            .chars()
            .map(|value| crate::translate::StyledCharacter { value, bold: true })
            .collect::<Vec<_>>();
        let content_objects = BTreeSet::from([(356, 0)]);

        let blocked = plan_text_segment(
            &chars,
            &translated,
            &content_objects,
            &output_fonts,
            page_bounds,
            &[phantom],
            None,
            None,
            None,
        );
        assert!(
            matches!(
                blocked,
                Err(TypesetPlanError::Preserved(
                    il::PreservedReason::TypesetOverflow
                ))
            ),
            "a visible obstacle covering the heading's own footprint must still degrade"
        );

        // 幽灵被判为不可见后它不再进入障碍集，同一段落照常排下译文。
        let planned = plan_text_segment(
            &chars,
            &translated,
            &content_objects,
            &output_fonts,
            page_bounds,
            &[],
            None,
            None,
            None,
        )
        .unwrap_or_else(|_| panic!("the heading fits its own footprint once the phantom is gone"));
        assert_eq!(planned.lines.len(), 1);
        assert!(planned.font_size >= MIN_FONT_SIZE_PT);
        assert!(
            !ink_bounds_are_safe(&planned.ink_bounds, page_bounds, &[phantom]),
            "the planned ink really does sit under the phantom"
        );
        assert!(ink_bounds_are_safe(&planned.ink_bounds, page_bounds, &[]));
    }

    #[test]
    fn typeset_overflow_detail_reports_the_container_obstacles_and_font_sizes() {
        let container = Rect {
            left: 108.0,
            bottom: 707.5383632,
            right: 230.767_948_8,
            top: 718.286_088,
        };
        let phantom = Rect {
            left: 109.110_816_955_566_4,
            bottom: 711.335_205_078_125,
            right: 234.453_857_421_875,
            top: 726.445_617_675_781_2,
        };
        let far_away = Rect {
            left: 0.0,
            bottom: 0.0,
            right: 10.0,
            top: 10.0,
        };
        let paragraph = Paragraph {
            reading_order: 69,
            bounds: container,
            first_line_indent: None,
            text: TextCarrier::Chars {
                chars: vec![Char {
                    unicode: Some('A'),
                    unicode_source: None,
                    code: 65,
                    visible: true,
                    font: crate::il::FontRef {
                        resource_name: "F87".to_owned(),
                        object_number: 105,
                        generation: 0,
                    },
                    font_size: 11.9552,
                    baseline_origin: crate::il::Point {
                        x: 108.0,
                        y: 710.037,
                    },
                    r#box: container,
                    visual_bbox: container,
                    bbox_estimated: false,
                    text_transform: TextTransform::Upright,
                    implicit_space_before: false,
                    layout: Some(LayoutAssignment {
                        label: LayoutLabel::FallbackLine,
                        reading_order: 288,
                        bounds: container,
                        source: LayoutSource::FallbackLine,
                        policy: TranslationPolicy::Translate,
                    }),
                    passthrough: PassthroughRef {
                        content_object: 356,
                        byte_start: 50,
                        byte_end: 87,
                        encoded: vec![65],
                    },
                }],
            },
            translated_text: None,
            translation_conservation: None,
            preserved: None,
        };

        let Diagnostic::TypesetOverflowDetail {
            page_index,
            paragraph_index,
            container: reported_container,
            attempted_font_sizes_pt,
            obstacle_count,
            obstacles,
        } = typeset_overflow_detail(12, &paragraph, &[far_away, phantom])
        else {
            panic!("typeset_overflow_detail must produce its own diagnostic");
        };
        assert_eq!(page_index, 12);
        assert_eq!(paragraph_index, 69);
        assert_eq!(
            reported_container,
            [
                container.left,
                container.bottom,
                container.right,
                container.top
            ]
        );
        assert_eq!(
            attempted_font_sizes_pt,
            vec![
                11.9552, 11.4552, 10.9552, 10.4552, 9.9552, 9.4552, 8.9552, 8.4552, 8.0
            ]
        );
        assert_eq!(obstacle_count, 1, "only container-overlapping obstacles");
        assert_eq!(
            obstacles,
            vec![[phantom.left, phantom.bottom, phantom.right, phantom.top]]
        );
    }

    #[test]
    fn normal_text_obstacles_include_retained_vector_and_inline_image_ink() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let translator = CountingTranslator::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let content_object = document.extracted_pages[0].content_streams[0].object_id;
        document.extracted_pages[0].vector_paths = vec![crate::walk::WalkedVectorPath {
            content_object,
            byte_start: 1,
            byte_end: 2,
            start: Point { x: 60.0, y: 110.0 },
            end: Point { x: 180.0, y: 110.0 },
            content_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            safe_to_replay: true,
            form_clip: None,
            clips: Default::default(),
        }];
        let image = Rect {
            left: 190.0,
            bottom: 100.0,
            right: 220.0,
            top: 125.0,
        };
        document.extracted_pages[0].inline_images = vec![crate::walk::WalkedInlineImage {
            content_object,
            byte_start: 3,
            byte_end: 4,
            bounds: image,
            content_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            replayable: true,
            form_clip: None,
            clips: Default::default(),
        }];

        let obstacles = paragraph_typeset_obstacles(
            &document.il.pages[0],
            &document.extracted_pages[0],
            &document.il.pages[0].paragraphs[0],
            true,
            &BTreeSet::new(),
        );

        assert!(obstacles.contains(&image));
        assert!(obstacles.contains(&Rect {
            left: 60.0,
            bottom: 109.99,
            right: 180.0,
            top: 110.01,
        }));
    }

    #[test]
    fn text_underline_requires_safe_unique_ownership_and_one_output_delta() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let translator = CountingTranslator::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let paragraph = &document.il.pages[0].paragraphs[0];
        let owner_bounds = paragraph
            .chars()
            .iter()
            .map(|character| character.r#box)
            .reduce(Rect::union)
            .unwrap();
        let owner_origin = paragraph.chars()[0].baseline_origin;
        let owner_span = (
            document.extracted_pages[0].content_streams[0].object_id,
            paragraph.chars()[0].passthrough.byte_start,
            paragraph.chars()[0].passthrough.byte_end,
        );
        let path = crate::walk::WalkedVectorPath {
            content_object: owner_span.0,
            byte_start: owner_span.1.saturating_sub(1),
            byte_end: owner_span.1,
            start: Point {
                x: owner_bounds.left,
                y: owner_origin.y - 2.0,
            },
            end: Point {
                x: owner_bounds.right,
                y: owner_origin.y - 2.0,
            },
            content_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            safe_to_replay: true,
            form_clip: None,
            clips: Default::default(),
        };
        document.extracted_pages[0].vector_paths = vec![path.clone()];
        let content_objects = BTreeSet::from([owner_span.0]);
        let underlines = uniquely_owned_text_underlines(
            paragraph,
            &document.extracted_pages[0],
            &content_objects,
        )
        .expect("the complete safe show owns its exact underline");
        assert_eq!(underlines.len(), 1);

        let mut plans = vec![TypesetPlan {
            spans: vec![owner_span],
            lines: vec![vec![crate::translate::StyledCharacter {
                value: '中',
                bold: false,
            }]],
            baselines: vec![(owner_origin.x - 18.0, owner_origin.y)],
            section_number_gap: None,
            formula_relocations: Vec::new(),
            text_vector_relocations: Vec::new(),
            ink_bounds: Vec::new(),
            font_size: 12.0,
            single_line_expansion: None,
            multi_line_expansion: None,
        }];
        attach_text_underlines_to_plans(
            &mut plans,
            &underlines,
            Rect {
                left: 0.0,
                bottom: 0.0,
                right: 300.0,
                top: 200.0,
            },
            Rect {
                left: owner_bounds.left - 20.0,
                bottom: owner_bounds.bottom - 5.0,
                right: owner_bounds.right,
                top: owner_bounds.top + 5.0,
            },
            &[],
        )
        .expect("one output line gives the owner one determinate delta");
        assert_eq!(plans[0].text_vector_relocations.len(), 1);
        assert_eq!(plans[0].text_vector_relocations[0].delta_x_pt, -18.0);
        assert_eq!(plans[0].text_vector_relocations[0].delta_y_pt, 0.0);
        assert!((plans[0].ink_bounds[0].left - (owner_bounds.left - 18.0)).abs() < 0.001);

        plans[0].text_vector_relocations.clear();
        plans[0].ink_bounds.clear();
        assert!(
            attach_text_underlines_to_plans(
                &mut plans,
                &underlines,
                Rect {
                    left: 0.0,
                    bottom: 0.0,
                    right: 300.0,
                    top: 200.0,
                },
                Rect {
                    left: owner_bounds.left - 17.0,
                    bottom: owner_bounds.bottom - 5.0,
                    right: owner_bounds.right,
                    top: owner_bounds.top + 5.0,
                },
                &[],
            )
            .is_none(),
            "a relocated underline outside its owning paragraph container must fail closed"
        );

        document.extracted_pages[0].vector_paths[0].safe_to_replay = false;
        assert!(
            uniquely_owned_text_underlines(
                paragraph,
                &document.extracted_pages[0],
                &content_objects,
            )
            .is_none(),
            "a suspicious underline that cannot be replayed independently must fail closed"
        );

        document.extracted_pages[0].vector_paths[0].safe_to_replay = true;
        document.extracted_pages[0].vector_paths[0].end.x =
            owner_bounds.left + (owner_bounds.right - owner_bounds.left) * 0.5;
        assert!(
            uniquely_owned_text_underlines(
                paragraph,
                &document.extracted_pages[0],
                &content_objects,
            )
            .expect("a short neighboring rule is not suspicious ownership")
            .is_empty(),
            "a rule that is too short to be an underline must remain an ordinary obstacle"
        );
    }

    #[test]
    fn retained_fill_collision_uses_polygon_geometry_and_background_coverage() {
        let segment = |start, end| crate::walk::WalkedPathSegment { start, end };
        let glyph = Rect {
            left: 19.0,
            bottom: 18.0,
            right: 30.0,
            top: 25.0,
        };
        let hull_only = crate::walk::WalkedPathInk {
            segments: vec![
                segment(Point { x: 10.0, y: 30.0 }, Point { x: 20.0, y: 30.0 }),
                segment(Point { x: 20.0, y: 30.0 }, Point { x: 10.0, y: 20.0 }),
                segment(Point { x: 10.0, y: 20.0 }, Point { x: 10.0, y: 30.0 }),
                segment(Point { x: 30.0, y: 10.0 }, Point { x: 40.0, y: 10.0 }),
                segment(Point { x: 40.0, y: 10.0 }, Point { x: 40.0, y: 0.0 }),
                segment(Point { x: 40.0, y: 0.0 }, Point { x: 30.0, y: 10.0 }),
            ],
            bounds: Rect {
                left: 10.0,
                bottom: 0.0,
                right: 40.0,
                top: 30.0,
            },
            filled: true,
            even_odd: false,
            stroke_radius: 0.0,
            form_clip: None,
            clips: Default::default(),
            replay_scope: None,
        };
        assert!(!retained_path_intersects_rect(&hull_only, glyph));

        let partial = crate::walk::WalkedPathInk {
            segments: vec![
                segment(Point { x: 20.0, y: 18.0 }, Point { x: 25.0, y: 18.0 }),
                segment(Point { x: 25.0, y: 18.0 }, Point { x: 25.0, y: 25.0 }),
                segment(Point { x: 25.0, y: 25.0 }, Point { x: 20.0, y: 25.0 }),
                segment(Point { x: 20.0, y: 25.0 }, Point { x: 20.0, y: 18.0 }),
            ],
            bounds: Rect {
                left: 20.0,
                bottom: 18.0,
                right: 25.0,
                top: 25.0,
            },
            filled: true,
            even_odd: false,
            stroke_radius: 0.0,
            form_clip: None,
            clips: Default::default(),
            replay_scope: None,
        };
        assert!(retained_path_intersects_rect(&partial, glyph));
        let mut clipped_partial = partial.clone();
        clipped_partial.clips.push(crate::walk::WalkedClipPath {
            segments: vec![
                segment(Point { x: 0.0, y: 30.0 }, Point { x: 100.0, y: 30.0 }),
                segment(Point { x: 100.0, y: 30.0 }, Point { x: 100.0, y: 40.0 }),
                segment(Point { x: 100.0, y: 40.0 }, Point { x: 0.0, y: 40.0 }),
                segment(Point { x: 0.0, y: 40.0 }, Point { x: 0.0, y: 30.0 }),
            ],
            bounds: Some(Rect {
                left: 0.0,
                bottom: 30.0,
                right: 100.0,
                top: 40.0,
            }),
            even_odd: false,
        });
        assert!(!retained_path_intersects_rect(&clipped_partial, glyph));

        let background = crate::walk::WalkedPathInk {
            segments: vec![
                segment(Point { x: 0.0, y: 0.0 }, Point { x: 100.0, y: 0.0 }),
                segment(Point { x: 100.0, y: 0.0 }, Point { x: 100.0, y: 100.0 }),
                segment(Point { x: 100.0, y: 100.0 }, Point { x: 0.0, y: 100.0 }),
                segment(Point { x: 0.0, y: 100.0 }, Point { x: 0.0, y: 0.0 }),
            ],
            bounds: Rect {
                left: 0.0,
                bottom: 0.0,
                right: 100.0,
                top: 100.0,
            },
            filled: true,
            even_odd: false,
            stroke_radius: 0.0,
            form_clip: None,
            clips: Default::default(),
            replay_scope: None,
        };
        assert!(retained_fill_contains_rect(
            &background,
            Rect {
                left: 10.0,
                bottom: 10.0,
                right: 90.0,
                top: 90.0,
            }
        ));
        let mut clipped_background = background;
        clipped_background.clips = clipped_partial.clips;
        assert!(!retained_fill_contains_rect(
            &clipped_background,
            Rect {
                left: 10.0,
                bottom: 10.0,
                right: 90.0,
                top: 90.0,
            }
        ));
    }

    #[test]
    fn cjk_kinsoku_tokens_keep_opening_and_closing_punctuation_off_breaks() {
        let styled = |text: &str| {
            text.chars()
                .map(|value| crate::translate::StyledCharacter { value, bold: false })
                .collect::<Vec<_>>()
        };
        let tokens = styled_text_tokens(&styled("甲乙丙（）丁戊己。庚"))
            .into_iter()
            .map(|token| token.into_iter().map(|character| character.value).collect())
            .collect::<Vec<String>>();
        assert_eq!(tokens, ["甲", "乙", "丙", "（）", "丁", "戊", "己。", "庚"]);
        let spaced = styled_text_tokens(&styled("甲 ，乙（ 丙"))
            .into_iter()
            .map(|token| token.into_iter().map(|character| character.value).collect())
            .collect::<Vec<String>>();
        assert_eq!(spaced, ["甲 ，", "乙", "（ 丙"]);
        assert!(!styled_text_kinsoku_is_satisfiable(&styled("）甲")));
        assert!(!styled_text_kinsoku_is_satisfiable(&styled("甲（")));
        assert!(styled_text_kinsoku_is_satisfiable(&styled("甲（）乙")));
    }

    #[test]
    fn retained_section_number_and_first_title_token_stay_on_the_same_line() {
        let styled = |text: &str| {
            text.chars()
                .map(|value| crate::translate::StyledCharacter { value, bold: false })
                .collect::<Vec<_>>()
        };
        let output_fonts = test_output_fonts();
        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let assert_wrapping = |text: &str, prefix_glyph_count: usize, expected_first: &str| {
            let text = styled(text);
            let gap = PlannedSectionNumberGap {
                prefix_glyph_count,
                prefix_in_output: true,
                source_prefix_left: 72.0,
                source_title_left: 90.0,
                output_prefix_width: 6.0,
                output_title_left: 90.0,
                gap_pt: 12.0,
                clamped: false,
            };
            let first_token = section_aware_styled_text_tokens(&text, Some(gap))[0].clone();
            let width = styled_token_width(&first_token, &faces, 12.0).unwrap() + gap.gap_pt;
            let lines = wrap_styled_text(&text, &faces, 12.0, width + 0.01, 0.0, Some(gap))
                .expect("the retained prefix and first title token fit together");
            let first = lines[0]
                .iter()
                .map(|character| character.value)
                .collect::<String>();
            assert_eq!(first, expected_first);
            assert!(lines.len() > 1, "the remaining title should wrap");
        };

        assert_wrapping("1标题", 1, "1标");
        assert_wrapping("1 标题", 2, "1 标");
    }

    #[test]
    fn independent_section_number_allows_formula_before_the_first_translated_segment() {
        let styled = |text: &str| {
            text.chars()
                .map(|value| crate::translate::StyledCharacter { value, bold: false })
                .collect::<Vec<_>>()
        };
        let output_fonts = test_output_fonts();
        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let source = SourceSectionNumberGeometry {
            prefix_glyph_count: 1,
            prefix_in_output: false,
            source_prefix_left: 72.0,
            source_prefix_right: 78.0,
            source_title_left: 90.0,
        };

        let gap = planned_section_number_gap(&[], &faces, 12.0, source)
            .expect("an independently retained prefix does not require translated prefix glyphs");
        assert!(!gap.prefix_in_output);
        assert_eq!(gap.output_prefix_width, 6.0);
        assert_eq!(gap.gap_pt, 12.0);
        assert_eq!(gap.output_title_left, 90.0);

        let shared = SourceSectionNumberGeometry {
            prefix_in_output: true,
            ..source
        };
        assert!(planned_section_number_gap(&[], &faces, 12.0, shared).is_none());
        let shared_gap = planned_section_number_gap(&styled("1"), &faces, 12.0, shared)
            .expect("a shared prefix may be followed immediately by a formula");
        let atoms = formula_flow_atoms(&[styled("1"), styled("标题")], Some(shared_gap));
        let FormulaFlowAtom::Text {
            extra_advance_pt,
            section_prefix_only,
            ..
        } = &atoms[0]
        else {
            panic!("the shared section prefix is the first formula-flow atom");
        };
        assert!(*section_prefix_only);
        assert!((*extra_advance_pt - shared_gap.gap_pt).abs() <= 0.001);
        assert!(matches!(atoms[1], FormulaFlowAtom::Formula(0)));
        assert!(formula_flow_atoms_must_stay_together(&atoms[0], &atoms[1]));
    }

    #[test]
    fn single_line_fit_always_tests_the_exact_minimum_font_size() {
        let translator = StaticTranslator {
            output: "中文测试中文测试",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let mut document =
            translate_fixture_once(&translator, &events, config_with_test_output_fonts()).unwrap();
        for character in match &mut document.il.pages[0].paragraphs[0].text {
            TextCarrier::Chars { chars } => chars,
        } {
            character.font_size = 9.9626;
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 119.0,
                right: 136.2,
                top: 129.0,
            };
        }
        let layout = document.il.pages[0].paragraphs[0].chars()[0]
            .layout
            .unwrap();
        document.extracted_pages[0].layout_regions[0].bounds = layout.bounds;
        document.extracted_pages[0].layout_regions[0].reading_order = layout.reading_order;
        document.extracted_pages[0].layout_regions[0].label = layout.label;
        document.extracted_pages[0].layout_regions[0].source = layout.source;
        let engine = FakeEngine::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };

        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        assert_eq!(
            document.rewrites[0]
                .typeset_characters
                .iter()
                .map(|character| character.unicode)
                .collect::<String>(),
            "中文测试中文测试"
        );
    }

    #[test]
    fn unsafe_fit_preserves_the_paragraph_without_tiny_text() {
        let mut document = Document::new(fixture(), "unused.pdf");
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CjkTranslator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        for pass in [parse as Pass, scan_detect, layout, paragraph_find] {
            pass(&mut document, &context).unwrap();
        }
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();
        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::TypesetOverflow)
        );
        assert!(document.il.pages[0].paragraphs[0].translated_text.is_none());
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
            characters: None,
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
    fn mixed_output_validation_accepts_preserved_and_typeset_characters() {
        let retained = [ExpectedOutputCharacter {
            unicode: Some('A'),
            baseline_origin: Point { x: 10.0, y: 20.0 },
        }];
        let typeset = [TypesetCharacter {
            unicode: '中',
            baseline_origin: Point { x: 30.0, y: 20.0 },
        }];
        let template = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let mut translated = template.clone();
        translated.unicode = Some('中');
        translated.unicode_value = u32::from('中');
        translated.baseline_origin = Point { x: 30.0, y: 20.0 };
        let mut preserved = template;
        preserved.unicode = Some('A');
        preserved.unicode_value = u32::from('A');
        preserved.baseline_origin = Point { x: 10.0, y: 20.0 };

        validate_mixed_output_characters(
            0,
            &retained,
            &typeset,
            &[translated.clone(), preserved.clone()],
            0.001,
        )
        .unwrap();

        preserved.baseline_origin.x += 0.01;
        let error = validate_mixed_output_characters(
            0,
            &retained,
            &typeset,
            &[translated, preserved],
            0.001,
        )
        .unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Internal(InternalReason::OutputMismatch)
        );
    }

    #[test]
    fn mixed_output_validation_allows_pdfium_to_omit_typeset_whitespace() {
        let retained = [ExpectedOutputCharacter {
            unicode: Some('A'),
            baseline_origin: Point { x: 10.0, y: 20.0 },
        }];
        let typeset = [
            TypesetCharacter {
                unicode: ' ',
                baseline_origin: Point { x: 20.0, y: 20.0 },
            },
            TypesetCharacter {
                unicode: '中',
                baseline_origin: Point { x: 30.0, y: 20.0 },
            },
        ];
        let template = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let mut preserved = template.clone();
        preserved.unicode = Some('A');
        preserved.unicode_value = u32::from('A');
        preserved.baseline_origin = retained[0].baseline_origin;
        let mut translated = template;
        translated.unicode = Some('中');
        translated.unicode_value = u32::from('中');
        translated.baseline_origin = typeset[1].baseline_origin;

        validate_mixed_output_characters(0, &retained, &typeset, &[preserved, translated], 0.001)
            .unwrap();
    }

    #[test]
    fn mixed_output_validation_still_requires_retained_whitespace() {
        let retained = [ExpectedOutputCharacter {
            unicode: Some(' '),
            baseline_origin: Point { x: 10.0, y: 20.0 },
        }];

        let error = validate_mixed_output_characters(0, &retained, &[], &[], 0.001).unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Internal(InternalReason::OutputMismatch)
        );
    }

    #[test]
    fn mixed_output_validation_still_requires_non_whitespace_typeset_characters() {
        let typeset = [TypesetCharacter {
            unicode: '中',
            baseline_origin: Point { x: 10.0, y: 20.0 },
        }];

        let error = validate_mixed_output_characters(0, &[], &typeset, &[], 0.001).unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Internal(InternalReason::OutputMismatch)
        );
    }

    #[test]
    fn mixed_output_validation_rejects_unexpected_extracted_characters() {
        let actual = FakeEngine::default().page_characters(&[], 0).unwrap();

        let error = validate_mixed_output_characters(0, &[], &[], &actual, 0.001).unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Internal(InternalReason::OutputMismatch)
        );
    }

    #[test]
    fn mixed_output_validation_accepts_pdfium_line_end_hyphen_marker() {
        let expected = TypesetCharacter {
            unicode: '-',
            baseline_origin: Point { x: 10.0, y: 20.0 },
        };
        let actual = PageCharSnapshot {
            index: 0,
            unicode: Some('\u{2}'),
            unicode_value: 2,
            is_hyphen: None,
            baseline_origin: expected.baseline_origin,
            tight_box: Rect::default(),
            loose_box: Rect::default(),
        };

        validate_mixed_output_characters(0, &[], &[expected], &[actual], 0.001).unwrap();
    }

    #[test]
    fn mixed_output_validation_prefers_exact_hyphen_over_pdfium_marker() {
        let expected = TypesetCharacter {
            unicode: '-',
            baseline_origin: Point { x: 10.0, y: 20.0 },
        };
        let marker = PageCharSnapshot {
            index: 0,
            unicode: Some('\u{2}'),
            unicode_value: 2,
            is_hyphen: None,
            baseline_origin: expected.baseline_origin,
            tight_box: Rect::default(),
            loose_box: Rect::default(),
        };
        let exact = PageCharSnapshot {
            index: 1,
            unicode: Some('-'),
            unicode_value: u32::from('-'),
            ..marker.clone()
        };

        validate_mixed_output_characters(0, &[], &[expected], &[marker, exact], 0.001).unwrap();
    }

    #[test]
    fn mixed_output_validation_accepts_retained_marker_as_output_hyphen() {
        let retained = ExpectedOutputCharacter {
            unicode: Some('\u{2}'),
            baseline_origin: Point { x: 10.0, y: 20.0 },
        };
        let actual = PageCharSnapshot {
            index: 0,
            unicode: Some('-'),
            unicode_value: u32::from('-'),
            is_hyphen: None,
            baseline_origin: retained.baseline_origin,
            tight_box: Rect::default(),
            loose_box: Rect::default(),
        };

        validate_mixed_output_characters(0, &[retained], &[], &[actual], 0.001).unwrap();
    }

    #[test]
    fn mixed_output_validation_consumes_both_pdfium_utf16_surrogates() {
        let expected = TypesetCharacter {
            unicode: '\u{1D44E}',
            baseline_origin: Point { x: 10.0, y: 20.0 },
        };
        let template = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let mut units = [0u16; 2];
        let encoded = expected.unicode.encode_utf16(&mut units);
        assert_eq!(encoded.len(), 2);
        let actual = encoded
            .iter()
            .enumerate()
            .map(|(index, unit)| PageCharSnapshot {
                index: index as u32,
                unicode: None,
                unicode_value: u32::from(*unit),
                baseline_origin: expected.baseline_origin,
                ..template.clone()
            })
            .collect::<Vec<_>>();

        validate_mixed_output_characters(0, &[], &[expected], &actual, 0.001).unwrap();
    }

    #[test]
    fn mixed_output_validation_ignores_pdfium_c0_extraction_markers() {
        let marker = ExpectedOutputCharacter {
            unicode: Some('\u{2}'),
            baseline_origin: Point { x: 10.0, y: 20.0 },
        };
        let mut actual_marker = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        actual_marker.unicode = Some('\u{2}');
        actual_marker.unicode_value = 2;
        actual_marker.baseline_origin = marker.baseline_origin;

        validate_mixed_output_characters(0, &[marker], &[], &[], 0.001).unwrap();
        validate_mixed_output_characters(0, &[], &[], &[actual_marker], 0.001).unwrap();
    }

    #[test]
    fn mixed_output_validation_accepts_coincident_pdfium_extraction_multiplicity() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../corpus/fixtures/unit-type-15-coincident-typeset/unit-type-15-coincident-typeset.pdf",
        );
        let candidate = std::fs::read(fixture).unwrap();
        let pdfium = crate::engine::pdfium::PdfiumEngine::from_environment().unwrap();
        let actual = pdfium.page_characters(&candidate, 0).unwrap();
        let coincident = TypesetCharacter {
            unicode: 'M',
            baseline_origin: Point { x: 72.0, y: 120.0 },
        };

        assert_eq!(actual.len(), 1, "PDFium folds the two coincident shows");
        validate_typeset_characters(0, &[coincident.clone(), coincident.clone()], &actual, 0.001)
            .unwrap();

        let missing_distinct_character = TypesetCharacter {
            unicode: 'I',
            ..coincident.clone()
        };
        assert!(
            validate_typeset_characters(
                0,
                &[coincident.clone(), missing_distinct_character],
                &actual,
                0.001,
            )
            .is_err()
        );
        let missing_distinct_position = TypesetCharacter {
            baseline_origin: Point { x: 73.0, y: 120.0 },
            ..coincident.clone()
        };
        assert!(
            validate_typeset_characters(
                0,
                &[coincident, missing_distinct_position],
                &actual,
                0.001,
            )
            .is_err()
        );
    }

    #[test]
    fn exact_unicode_sequences_can_attribute_rewrites_without_baseline_links() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let walked = walk_page(&pdf, page_id).unwrap().characters;
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();

        assert!(exact_unicode_sequence(&walked, &engine));
        assert_eq!(
            sequence_engine_indices_by_walk(&walked, &engine).unwrap(),
            vec![Some(0), Some(1), Some(2), Some(3), Some(4)]
        );
        let mut marker = engine[1].clone();
        marker.unicode = None;
        marker.unicode_value = 0;
        engine.insert(2, marker);
        assert_eq!(
            sequence_engine_indices_by_walk(&walked, &engine).unwrap(),
            vec![Some(0), Some(1), Some(3), Some(4), Some(5)]
        );
        engine.remove(2);
        engine[0].unicode = Some('X');
        assert!(!exact_unicode_sequence(&walked, &engine));
        engine.pop();
        assert!(!exact_unicode_sequence(&walked, &engine));
    }

    #[test]
    fn retained_character_owners_extend_across_same_line_width_drift() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let walked = walk_page(&pdf, page_id).unwrap().characters;
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        engine[1].baseline_origin.x += 1.0;
        engine[2].baseline_origin.x += 2.0;
        let mut owners = vec![Some(0), None, None];

        extend_engine_owners_by_contiguous_sequence(&walked[..3], &engine[..3], &mut owners, 0.001);

        assert_eq!(owners, [Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn pdfium_utf16_surrogate_pairs_share_one_walk_owner() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut walked = walk_page(&pdf, page_id).unwrap().characters;
        walked.truncate(1);
        walked[0].unicode = Some('\u{1D11E}');
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        engine.truncate(2);
        let mut utf16 = [0u16; 2];
        let encoded = '\u{1D11E}'.encode_utf16(&mut utf16);
        engine[0].unicode = None;
        engine[0].unicode_value = u32::from(encoded[0]);
        engine[1].unicode = None;
        engine[1].unicode_value = u32::from(encoded[1]);
        let mut owners = vec![Some(0), None];

        inherit_pdfium_utf16_surrogate_owners(&walked, &engine, &mut owners);

        assert_eq!(owners, [Some(0), Some(0)]);
    }

    #[test]
    fn pdfium_ligature_expansion_components_share_one_walk_owner() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut walked = walk_page(&pdf, page_id).unwrap().characters;
        walked.truncate(1);
        walked[0].unicode = Some('\u{FB03}');
        let template = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let engine = ['e', 'f', 'f', 'i', 'c']
            .into_iter()
            .enumerate()
            .map(|(index, unicode)| PageCharSnapshot {
                index: index as u32,
                unicode: Some(unicode),
                unicode_value: u32::from(unicode),
                ..template.clone()
            })
            .collect::<Vec<_>>();
        // Sequence alignment may choose either repeated `f`; all three PDFium
        // extraction components still came from the one source `ffi` glyph.
        let mut owners = vec![None, None, Some(0), None, None];

        inherit_pdfium_ligature_expansion_owners(&walked, &engine, &mut owners);

        assert_eq!(owners, [None, Some(0), Some(0), Some(0), None]);
    }

    #[test]
    fn retained_character_owners_continue_after_ligature_expansion() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut walked = walk_page(&pdf, page_id).unwrap().characters;
        walked.truncate(2);
        walked[0].unicode = Some('\u{FB03}');
        walked[1].unicode = Some('c');
        let template = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let engine = ['f', 'f', 'i', 'c']
            .into_iter()
            .enumerate()
            .map(|(index, unicode)| PageCharSnapshot {
                index: index as u32,
                unicode: Some(unicode),
                unicode_value: u32::from(unicode),
                baseline_origin: Point {
                    x: index as f64,
                    y: walked[index.min(1)].baseline_origin.y,
                },
                ..template.clone()
            })
            .collect::<Vec<_>>();
        let mut owners = vec![None, Some(0), None, None];

        inherit_pdfium_ligature_expansion_owners(&walked, &engine, &mut owners);
        extend_engine_owners_by_contiguous_sequence(&walked, &engine, &mut owners, 0.001);

        assert_eq!(owners, [Some(0), Some(0), Some(0), Some(1)]);
    }

    #[test]
    fn ambiguous_pdfium_ligature_expansion_is_not_claimed() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut walked = walk_page(&pdf, page_id).unwrap().characters;
        walked.truncate(1);
        walked[0].unicode = Some('\u{FB00}');
        let template = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let engine = ['f', 'f', 'f']
            .into_iter()
            .enumerate()
            .map(|(index, unicode)| PageCharSnapshot {
                index: index as u32,
                unicode: Some(unicode),
                unicode_value: u32::from(unicode),
                ..template.clone()
            })
            .collect::<Vec<_>>();
        let mut owners = vec![None, Some(0), None];

        inherit_pdfium_ligature_expansion_owners(&walked, &engine, &mut owners);

        assert_eq!(owners, [None, Some(0), None]);
    }

    #[test]
    fn retained_character_owners_absorb_unique_unresolved_explanations() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut walked = walk_page(&pdf, page_id).unwrap().characters[..3].to_vec();
        walked[1].unicode = None;
        walked[1].unicode_provenance = UnicodeProvenance::Unresolved;
        let engine = FakeEngine::default().page_characters(&[], 0).unwrap()[..3].to_vec();
        let mut owners = vec![Some(0), None, Some(2)];

        inherit_unique_explanation_owners(&walked, &engine, &mut owners, 0.001);

        assert_eq!(owners, [Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn ambiguous_unresolved_output_owners_share_their_span_modification_state() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut character = walk_page(&pdf, page_id).unwrap().characters[0].clone();
        character.unicode = None;
        character.unicode_provenance = UnicodeProvenance::Unresolved;
        character.baseline_origin = Point {
            x: 473.95,
            y: 284.462,
        };
        let mut second_character = character.clone();
        second_character.baseline_origin.x += 0.006_974;
        let walked = [character, second_character];

        let template = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let engine = ['u', 't'].map(|unicode| PageCharSnapshot {
            unicode: Some(unicode),
            unicode_value: u32::from(unicode),
            baseline_origin: Point {
                x: 473.950_012,
                y: 284.462_006,
            },
            ..template.clone()
        });
        let owners = [None, None];

        for (modified, expected) in [
            ([false, false], Some(false)),
            ([true, true], Some(true)),
            ([false, true], None),
        ] {
            let (states, has_walk_candidates) =
                engine_modification_states(&walked, &engine, &owners, &modified, 0.01);
            assert_eq!(states, [expected, expected]);
            assert_eq!(has_walk_candidates, [true, true]);
        }
    }

    #[test]
    fn typeset_matrix_compensates_for_the_active_content_transform() {
        let matrix =
            content_relative_text_matrix([1.0, 0.0, 0.0, -1.0, 30.0, 117.0], 25.0, 100.0).unwrap();

        assert_eq!(matrix, [1.0, 0.0, 0.0, -1.0, -5.0, 17.0]);
        assert!(content_relative_text_matrix([1.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.0, 0.0).is_none());
    }

    #[test]
    fn pdf_numbers_preserve_sub_millipoint_typeset_precision() {
        assert_eq!(pdf_number(107.582849), "107.582849");
        assert_eq!(pdf_number(9.941812), "9.941812");
        assert_eq!(pdf_number(1.0), "1");
    }

    #[test]
    fn typeset_replacement_closes_a_tj_array_before_emitting_text_operators() {
        let span = ((1, 0), 0, 8);
        let source = b"[<0041>]";
        let streams = BTreeMap::from([((1, 0), source.as_slice())]);
        let transforms = BTreeMap::from([(span, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0])]);
        let text_show_states = BTreeMap::from([(
            span,
            TextShowState {
                line_matrix: [1.0, 0.0, 0.0, 1.0, 10.0, 20.0],
                matrix_after_show: [1.0, 0.0, 0.0, 1.0, 18.0, 20.0],
                font_size: 8.0,
                horizontal_scale: 1.0,
            },
        )]);
        let output_fonts = test_output_fonts();
        let (font, cids) = build_embedded_font(
            &BTreeSet::from(['中']),
            &output_fonts.regular,
            OutputFontKey::PrimaryRegular,
        )
        .unwrap();
        let fonts = BTreeMap::from([(
            OutputFontKey::PrimaryRegular,
            BuiltOutputFont { font, cids },
        )]);
        let plan = TypesetPlan {
            spans: vec![span],
            lines: vec![vec![crate::translate::StyledCharacter {
                value: '中',
                bold: false,
            }]],
            baselines: vec![(25.0, 100.0)],
            section_number_gap: None,
            formula_relocations: Vec::new(),
            text_vector_relocations: Vec::new(),
            ink_bounds: Vec::new(),
            font_size: 8.0,
            single_line_expansion: None,
            multi_line_expansion: None,
        };
        let mut replacements = BTreeMap::new();

        install_typeset_replacements(
            &plan,
            &fonts,
            &streams,
            &transforms,
            &text_show_states,
            &mut replacements,
        )
        .unwrap();
        install_typeset_replacements(
            &TypesetPlan {
                baselines: vec![(75.0, 100.0)],
                ..plan
            },
            &fonts,
            &streams,
            &transforms,
            &text_show_states,
            &mut replacements,
        )
        .unwrap();

        let replacement = std::str::from_utf8(&replacements[&span]).unwrap();
        assert!(replacement.starts_with("[] TJ\n"), "{replacement}");
        assert!(
            replacement.contains("0 Tc 0 Tw 100 Tz 0 Ts 0 Tr\n"),
            "{replacement}"
        );
        assert!(replacement.contains(" Tm "), "{replacement}");
        assert_eq!(replacement.matches("/MimusR").count(), 2, "{replacement}");
        assert_eq!(replacement.matches("q\n").count(), 1, "{replacement}");
        assert_eq!(replacement.matches("Q\n").count(), 1, "{replacement}");
        assert!(
            replacement.ends_with("Q\n1 0 0 1 10 20 Tm\n[-1000] TJ\n[]"),
            "{replacement}"
        );
    }

    #[test]
    fn typeset_replacement_claims_a_neutralized_secondary_span() {
        let first = ((1, 0), 0, 8);
        let second = ((1, 0), 9, 17);
        let source = b"[<0041>] [<0042>]";
        let streams = BTreeMap::from([((1, 0), source.as_slice())]);
        let transforms = BTreeMap::from([
            (first, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            (second, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        ]);
        let state = TextShowState {
            line_matrix: [1.0, 0.0, 0.0, 1.0, 10.0, 20.0],
            matrix_after_show: [1.0, 0.0, 0.0, 1.0, 18.0, 20.0],
            font_size: 8.0,
            horizontal_scale: 1.0,
        };
        let text_show_states = BTreeMap::from([(first, state), (second, state)]);
        let output_fonts = test_output_fonts();
        let (font, cids) = build_embedded_font(
            &BTreeSet::from(['中', '文']),
            &output_fonts.regular,
            OutputFontKey::PrimaryRegular,
        )
        .unwrap();
        let fonts = BTreeMap::from([(
            OutputFontKey::PrimaryRegular,
            BuiltOutputFont { font, cids },
        )]);
        let plan = |spans, value, x| TypesetPlan {
            spans,
            lines: vec![vec![crate::translate::StyledCharacter {
                value,
                bold: false,
            }]],
            baselines: vec![(x, 100.0)],
            section_number_gap: None,
            formula_relocations: Vec::new(),
            text_vector_relocations: Vec::new(),
            ink_bounds: Vec::new(),
            font_size: 8.0,
            single_line_expansion: None,
            multi_line_expansion: None,
        };
        let mut replacements = BTreeMap::new();

        install_typeset_replacements(
            &plan(vec![first, second], '中', 25.0),
            &fonts,
            &streams,
            &transforms,
            &text_show_states,
            &mut replacements,
        )
        .unwrap();
        install_typeset_replacements(
            &plan(vec![second], '文', 75.0),
            &fonts,
            &streams,
            &transforms,
            &text_show_states,
            &mut replacements,
        )
        .unwrap();

        assert!(
            replacements[&first]
                .windows(4)
                .any(|bytes| bytes == b"0001")
        );
        assert!(
            replacements[&second]
                .windows(4)
                .any(|bytes| bytes == b"0002")
        );
    }

    #[test]
    fn incompatible_formula_and_text_replacements_preserve_the_connected_component() {
        let first = ((1, 0), 0, 8);
        let second = ((1, 0), 9, 17);
        let source = b"[<0041>] [<0042>]";
        let streams = BTreeMap::from([((1, 0), source.as_slice())]);
        let transforms = BTreeMap::from([
            (first, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            (second, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        ]);
        let state = TextShowState {
            line_matrix: [1.0, 0.0, 0.0, 1.0, 10.0, 20.0],
            matrix_after_show: [1.0, 0.0, 0.0, 1.0, 18.0, 20.0],
            font_size: 8.0,
            horizontal_scale: 1.0,
        };
        let text_show_states = BTreeMap::from([(first, state), (second, state)]);
        let output_fonts = test_output_fonts();
        let (font, cids) = build_embedded_font(
            &BTreeSet::from(['中', '文']),
            &output_fonts.regular,
            OutputFontKey::PrimaryRegular,
        )
        .unwrap();
        let fonts = BTreeMap::from([(
            OutputFontKey::PrimaryRegular,
            BuiltOutputFont { font, cids },
        )]);
        let text_plan = |span, value, x| TypesetPlan {
            spans: vec![span],
            lines: vec![vec![crate::translate::StyledCharacter {
                value,
                bold: false,
            }]],
            baselines: vec![(x, 100.0)],
            section_number_gap: None,
            formula_relocations: Vec::new(),
            text_vector_relocations: Vec::new(),
            ink_bounds: Vec::new(),
            font_size: 8.0,
            single_line_expansion: None,
            multi_line_expansion: None,
        };
        let first_paragraph = text_plan(first, '中', 25.0);
        let mut second_paragraph = text_plan(second, '文', 75.0);
        second_paragraph
            .formula_relocations
            .push(FormulaRelocation {
                spans: vec![first],
                split_glyphs: BTreeMap::new(),
                vector_paths: Vec::new(),
                inline_images: Vec::new(),
                delta_x_pt: 1.0,
                delta_y_pt: 0.0,
                characters: Vec::new(),
                text_ink_bounds: Rect::default(),
                glyph_ink_bounds: Vec::new(),
                source_fonts: Vec::new(),
            });
        let planned = vec![(0, vec![first_paragraph]), (1, vec![second_paragraph])];

        let incompatible = incompatible_plan_component_indices(
            &planned,
            &fonts,
            &streams,
            &transforms,
            &text_show_states,
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(incompatible, BTreeSet::from([0, 1]));
    }

    #[test]
    fn typeset_combines_identity_and_translated_paragraphs_that_share_one_operand() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let paragraph = document.il.pages[0].paragraphs.pop().unwrap();
        let TextCarrier::Chars { mut chars } = paragraph.text;
        let layout_bounds = Rect {
            left: 72.0,
            bottom: 90.0,
            right: 172.0,
            top: 135.0,
        };
        for character in &mut chars {
            character.layout.as_mut().unwrap().bounds = layout_bounds;
        }
        document.extracted_pages[0].layout_regions[0].bounds = layout_bounds;
        let shared_span = (
            chars[0].passthrough.content_object,
            chars[0].passthrough.byte_start,
            chars[0].passthrough.byte_end,
        );
        assert!(chars.iter().all(|character| {
            (
                character.passthrough.content_object,
                character.passthrough.byte_start,
                character.passthrough.byte_end,
            ) == shared_span
        }));
        document.il.pages[0].paragraphs = vec![
            Paragraph {
                reading_order: 0,
                bounds: layout_bounds,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[..1].to_vec(),
                },
                translated_text: Some("M".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
            Paragraph {
                reading_order: 1,
                bounds: layout_bounds,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[1..].to_vec(),
                },
                translated_text: Some("中文".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
        ];

        typeset(&mut document, &context).unwrap();

        assert_eq!(document.rewrites.len(), 1);
        let values = document.rewrites[0]
            .typeset_characters
            .iter()
            .map(|character| character.unicode)
            .collect::<String>();
        assert_eq!(values, "M中文");
    }

    #[test]
    fn formula_relocation_rejects_a_show_operand_not_fully_owned_by_its_paragraph() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();

        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let mut formula = paragraph.chars()[1].clone();
        let layout = formula.layout.as_mut().unwrap();
        layout.label = LayoutLabel::InlineFormula;
        layout.policy = TranslationPolicy::Passthrough;
        paragraph.text = TextCarrier::Chars {
            chars: vec![formula],
        };
        let extracted = &document.extracted_pages[0];
        let content_objects = extracted
            .content_streams
            .iter()
            .map(|stream| stream.object_id)
            .collect::<BTreeSet<_>>();
        let content_object_numbers = content_objects
            .iter()
            .map(|object_id| object_id.0)
            .collect::<BTreeSet<_>>();

        assert!(
            source_formula_units(
                paragraph,
                extracted,
                &content_objects,
                &content_object_numbers,
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn formula_relocation_rejects_other_passthrough_text_in_a_shared_operand() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();

        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        let formula_layout = chars[1].layout.as_mut().unwrap();
        formula_layout.label = LayoutLabel::InlineFormula;
        formula_layout.policy = TranslationPolicy::Passthrough;
        let passthrough_layout = chars[2].layout.as_mut().unwrap();
        passthrough_layout.label = LayoutLabel::Number;
        passthrough_layout.policy = TranslationPolicy::Passthrough;
        let extracted = &document.extracted_pages[0];
        let content_objects = extracted
            .content_streams
            .iter()
            .map(|stream| stream.object_id)
            .collect::<BTreeSet<_>>();
        let content_object_numbers = content_objects
            .iter()
            .map(|object_id| object_id.0)
            .collect::<BTreeSet<_>>();

        assert!(
            source_formula_units(
                paragraph,
                extracted,
                &content_objects,
                &content_object_numbers,
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn formula_relocation_rejects_a_multi_scalar_source_glyph() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();

        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        let formula = &mut chars[1];
        let layout = formula.layout.as_mut().unwrap();
        layout.label = LayoutLabel::InlineFormula;
        layout.policy = TranslationPolicy::Passthrough;
        document.extracted_pages[0].walked_characters[1].source_glyph_scalar_count = 2;
        let extracted = &document.extracted_pages[0];
        let content_objects = extracted
            .content_streams
            .iter()
            .map(|stream| stream.object_id)
            .collect::<BTreeSet<_>>();
        let content_object_numbers = content_objects
            .iter()
            .map(|object_id| object_id.0)
            .collect::<BTreeSet<_>>();

        assert!(
            source_formula_units(
                paragraph,
                extracted,
                &content_objects,
                &content_object_numbers,
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn formula_replay_validation_uses_the_input_engine_extraction_view() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let mut character = document.il.pages[0].paragraphs[0].chars()[0].clone();
        character.unicode = Some(';');
        let extracted = &mut document.extracted_pages[0];
        extracted.engine_characters[0].unicode = Some(',');
        extracted.engine_characters[0].unicode_value = u32::from(',');

        let expected = formula_validation_character(extracted, &character).unwrap();

        assert_eq!(expected.unicode, ',');
        assert_eq!(
            expected.baseline_origin,
            extracted.engine_characters[0].baseline_origin
        );
    }

    #[test]
    fn formula_replay_without_an_alignment_rejects_a_nearby_unicode_conflict() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let mut character = document.il.pages[0].paragraphs[0].chars()[0].clone();
        character.unicode = Some(';');
        let extracted = &mut document.extracted_pages[0];
        extracted.character_alignment.engine_indices_by_walk[0] = None;
        extracted.engine_characters[0].unicode = Some(',');
        extracted.engine_characters[0].unicode_value = u32::from(',');
        extracted.engine_characters[0].baseline_origin.x += 0.002;

        assert!(formula_validation_character(extracted, &character).is_none());
    }

    #[test]
    fn typeset_combines_passthrough_identity_prefix_with_shared_translated_text() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let paragraph = document.il.pages[0].paragraphs.pop().unwrap();
        let TextCarrier::Chars { mut chars } = paragraph.text;
        let layout_bounds = Rect {
            left: 72.0,
            bottom: 119.0,
            right: 172.0,
            top: 129.0,
        };
        for character in &mut chars {
            character.layout.as_mut().unwrap().bounds = layout_bounds;
        }
        chars[0].layout.as_mut().unwrap().label = LayoutLabel::Number;
        chars[0].layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        let number_layout = chars[0].layout.unwrap();
        document.extracted_pages[0].layout_regions[0].bounds = layout_bounds;
        document.extracted_pages[0].layout_regions[0].reading_order = number_layout.reading_order;
        document.extracted_pages[0].layout_regions[0].source = number_layout.source;
        document.extracted_pages[0].layout_regions[0].label = LayoutLabel::Text;
        document.il.pages[0].paragraphs = vec![
            Paragraph {
                reading_order: 0,
                bounds: chars[0].r#box,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[..1].to_vec(),
                },
                translated_text: Some("M".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
            Paragraph {
                reading_order: 1,
                bounds: chars[1..]
                    .iter()
                    .map(|character| character.r#box)
                    .reduce(Rect::union)
                    .unwrap(),
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[1..].to_vec(),
                },
                translated_text: Some("中文".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
        ];

        typeset(&mut document, &context).unwrap();

        assert!(
            document.il.pages[0]
                .paragraphs
                .iter()
                .all(|paragraph| paragraph.preserved.is_none())
        );
        let values = document.rewrites[0]
            .typeset_characters
            .iter()
            .map(|character| character.unicode)
            .collect::<String>();
        assert_eq!(values, "M中文");
    }

    #[test]
    fn typeset_retains_numeric_passthrough_prefix_inside_one_translated_paragraph() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("shared-section-title.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 120 Tm\n[(M) -100 (IMUS)] TJ\nET\n".to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let paragraph = document.il.pages[0].paragraphs.pop().unwrap();
        let TextCarrier::Chars { mut chars } = paragraph.text;
        let shared_span = (
            chars[0].passthrough.content_object,
            chars[0].passthrough.byte_start,
            chars[0].passthrough.byte_end,
        );
        assert!(chars.iter().all(|character| {
            (
                character.passthrough.content_object,
                character.passthrough.byte_start,
                character.passthrough.byte_end,
            ) == shared_span
        }));
        let layout_bounds = Rect {
            left: 72.0,
            bottom: 119.0,
            right: 172.0,
            top: 129.0,
        };
        for character in &mut chars {
            let layout = character.layout.as_mut().unwrap();
            layout.bounds = layout_bounds;
            layout.label = LayoutLabel::ParagraphTitle;
            layout.policy = TranslationPolicy::Translate;
        }
        chars[0].unicode = Some('1');
        chars[0].layout.as_mut().unwrap().label = LayoutLabel::Number;
        chars[0].layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        document.extracted_pages[0].layout_regions[0].bounds = layout_bounds;
        let source_prefix_left = chars[0].r#box.left;
        let source_title_left = chars[1].r#box.left;
        chars.swap(0, 1);
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: layout_bounds,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: Some("中文".to_owned()),
            translation_conservation: None,
            preserved: None,
        }];

        typeset(&mut document, &context).unwrap();

        assert!(document.il.pages[0].paragraphs[0].preserved.is_none());
        let values = document.rewrites[0]
            .typeset_characters
            .iter()
            .map(|character| character.unicode)
            .collect::<String>();
        assert_eq!(values, "1中文");
        let output_prefix_left = document.rewrites[0].typeset_characters[0].baseline_origin.x;
        let output_title_left = document.rewrites[0].typeset_characters[1].baseline_origin.x;
        assert!((output_prefix_left - source_prefix_left).abs() <= 0.001);
        assert!(output_title_left >= source_title_left);
        let evidence = document.il.publication_ink[0]
            .section_number_gap
            .expect("retained section-number geometry is published");
        assert_eq!(evidence.prefix_glyph_count, 1);
        assert!(evidence.clamped);
        assert!((evidence.gap_pt - evidence.font_size * 0.25).abs() <= 0.001);
        assert!((evidence.output_title_left - output_title_left).abs() <= 0.001);
        assert!(
            document
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| matches!(
                    diagnostic,
                    Diagnostic::SectionNumberGapClamped {
                        page_index: 0,
                        reading_order: 0,
                        ..
                    }
                ))
        );
    }

    #[test]
    fn typeset_positions_title_after_visually_leading_independent_source_section_number() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("independent-section-title.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 120 Tm\n(M) Tj\n1 0 0 1 94 120 Tm\n(IMUS) Tj\nET\n"
                    .to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let paragraph = document.il.pages[0].paragraphs.pop().unwrap();
        let TextCarrier::Chars { mut chars } = paragraph.text;
        let layout_bounds = Rect {
            left: 72.0,
            bottom: 119.0,
            right: 172.0,
            top: 129.0,
        };
        for character in &mut chars {
            let layout = character.layout.as_mut().unwrap();
            layout.bounds = layout_bounds;
            layout.label = LayoutLabel::ParagraphTitle;
            layout.policy = TranslationPolicy::Translate;
        }
        chars[0].unicode = Some('1');
        chars[0].layout.as_mut().unwrap().label = LayoutLabel::Number;
        chars[0].layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        document.extracted_pages[0].layout_regions[0].bounds = layout_bounds;
        let source_prefix_left = chars[0].r#box.left;
        let source_prefix_width = chars[0].r#box.right - source_prefix_left;
        let source_title_left = chars[1].r#box.left;
        let number_span = (
            chars[0].passthrough.content_object,
            chars[0].passthrough.byte_start,
            chars[0].passthrough.byte_end,
        );
        let title_span = (
            chars[1].passthrough.content_object,
            chars[1].passthrough.byte_start,
            chars[1].passthrough.byte_end,
        );
        assert_ne!(number_span, title_span);
        chars.swap(0, 1);
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: layout_bounds,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: Some("中文".to_owned()),
            translation_conservation: None,
            preserved: None,
        }];

        typeset(&mut document, &context).unwrap();

        assert!(document.il.pages[0].paragraphs[0].preserved.is_none());
        let rewrite = &document.rewrites[0];
        let values = rewrite
            .typeset_characters
            .iter()
            .map(|character| character.unicode)
            .collect::<String>();
        assert_eq!(values, "中文");
        assert!(
            (rewrite.typeset_characters[0].baseline_origin.x - source_title_left).abs() <= 0.001
        );
        assert!(rewrite.replacements.iter().all(|replacement| {
            (
                replacement.content_object.0,
                replacement.byte_start,
                replacement.byte_end,
            ) != number_span
        }));
        assert!(rewrite.replacements.iter().any(|replacement| {
            (
                replacement.content_object.0,
                replacement.byte_start,
                replacement.byte_end,
            ) == title_span
        }));
        let evidence = document.il.publication_ink[0]
            .section_number_gap
            .expect("independent section-number geometry is published");
        assert_eq!(evidence.prefix_glyph_count, 1);
        assert!(!evidence.prefix_in_output);
        assert!((evidence.source_prefix_left - source_prefix_left).abs() <= 0.001);
        assert!((evidence.output_prefix_width - source_prefix_width).abs() <= 0.001);
        assert!((evidence.output_title_left - source_title_left).abs() <= 0.001);
        assert!(!evidence.clamped);
    }

    #[test]
    fn independent_section_number_before_fixed_formula_publishes_gap_without_moving_formula() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory
            .path()
            .join("independent-number-formula-title.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 140 Tm\n(M) Tj\n1 0 0 1 90 140 Tm\n(U) Tj\n1 0 0 1 104 140 Tm\n(MIMUS) Tj\n1 0 0 1 104 126 Tm\n(MIMUS) Tj\nET\n"
                    .to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "{v1}<b1>\u{540e}\u{6587}</b1>",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        chars.sort_by(|left, right| {
            right
                .baseline_origin
                .y
                .total_cmp(&left.baseline_origin.y)
                .then_with(|| left.baseline_origin.x.total_cmp(&right.baseline_origin.x))
        });
        let owner = Rect {
            left: 72.0,
            bottom: 110.0,
            right: 220.0,
            top: 152.0,
        };
        for character in &mut chars {
            let layout = character.layout.as_mut().unwrap();
            layout.bounds = owner;
            layout.label = LayoutLabel::ParagraphTitle;
            layout.policy = TranslationPolicy::Translate;
        }
        document.extracted_pages[0].layout_regions[0].bounds = owner;

        let number_index = chars
            .iter()
            .position(|character| {
                character.unicode == Some('M') && (character.baseline_origin.x - 72.0).abs() <= 0.01
            })
            .unwrap();
        chars[number_index].unicode = Some('3');
        let number_layout = chars[number_index].layout.as_mut().unwrap();
        number_layout.label = LayoutLabel::Number;
        number_layout.policy = TranslationPolicy::Passthrough;
        let number_span = span_key(&chars[number_index], (9, 0));
        let source_prefix_left = chars[number_index].r#box.left;

        let formula_index = chars
            .iter()
            .position(|character| character.unicode == Some('U'))
            .unwrap();
        let formula_span = span_key(&chars[formula_index], (9, 0));
        let source_title_left = chars[formula_index].r#box.left;
        let formula_layout = chars[formula_index].layout.as_mut().unwrap();
        formula_layout.label = LayoutLabel::InlineFormula;
        formula_layout.policy = TranslationPolicy::Passthrough;
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: owner,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: None,
            translation_conservation: None,
            preserved: None,
        }];

        styles_and_formulas(&mut document, &context).unwrap();
        let request_text = document
            .prepared_translations
            .values()
            .next()
            .unwrap()
            .request_text();
        assert!(!request_text.contains('3'));
        assert!(request_text.starts_with("{v1}"), "{request_text:?}");
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        let rewrite = &document.rewrites[0];
        assert!(rewrite.replacements.iter().all(|replacement| {
            let span = (
                replacement.content_object,
                replacement.byte_start,
                replacement.byte_end,
            );
            span != number_span && span != formula_span
        }));
        let evidence = document.il.publication_ink[0]
            .section_number_gap
            .expect("the fixed formula-first title publishes section-number geometry");
        assert!(!evidence.prefix_in_output);
        assert!(!evidence.clamped);
        assert!((evidence.source_prefix_left - source_prefix_left).abs() <= 0.001);
        assert!((evidence.source_title_left - source_title_left).abs() <= 0.001);
        assert!((evidence.output_title_left - source_title_left).abs() <= 0.001);
    }

    #[test]
    fn unsupported_passthrough_prefix_inside_a_translated_operand_fails_closed() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let paragraph = document.il.pages[0].paragraphs.pop().unwrap();
        let TextCarrier::Chars { mut chars } = paragraph.text;
        let layout_bounds = Rect {
            left: 72.0,
            bottom: 119.0,
            right: 172.0,
            top: 129.0,
        };
        for character in &mut chars {
            let layout = character.layout.as_mut().unwrap();
            layout.bounds = layout_bounds;
            layout.label = LayoutLabel::ParagraphTitle;
            layout.policy = TranslationPolicy::Translate;
        }
        chars[0].layout.as_mut().unwrap().label = LayoutLabel::Number;
        chars[0].layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        document.extracted_pages[0].layout_regions[0].bounds = layout_bounds;
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: layout_bounds,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: Some("中文".to_owned()),
            translation_conservation: None,
            preserved: None,
        }];

        typeset(&mut document, &context).unwrap();

        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::TypesetProtocol)
        );
        assert!(document.rewrites.is_empty());
    }

    #[test]
    fn failed_shared_identity_preserves_only_its_span_component() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("two-spans.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\n0 -20 Td (MIMUS) Tj\nET\n".to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();
        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        chars.sort_by(|left, right| {
            right
                .baseline_origin
                .y
                .total_cmp(&left.baseline_origin.y)
                .then_with(|| left.baseline_origin.x.total_cmp(&right.baseline_origin.x))
        });
        assert_eq!(chars.len(), 10);
        let normal_bounds = Rect {
            left: 70.0,
            bottom: 90.0,
            right: 170.0,
            top: 135.0,
        };
        for character in &mut chars {
            character.layout.as_mut().unwrap().bounds = normal_bounds;
        }
        document.extracted_pages[0].layout_regions.clear();
        chars[0].layout.as_mut().unwrap().bounds.right = 70.1;
        assert_eq!(span_key(&chars[0], (9, 0)), span_key(&chars[4], (9, 0)));
        assert_ne!(span_key(&chars[0], (9, 0)), span_key(&chars[5], (9, 0)));
        document.il.pages[0].paragraphs = vec![
            Paragraph {
                reading_order: 0,
                bounds: chars[0].layout.unwrap().bounds,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[..1].to_vec(),
                },
                translated_text: Some("M".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
            Paragraph {
                reading_order: 1,
                bounds: normal_bounds,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[1..5].to_vec(),
                },
                translated_text: Some("中文".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
            Paragraph {
                reading_order: 2,
                bounds: normal_bounds,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[5..].to_vec(),
                },
                translated_text: Some("测试".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
        ];

        typeset(&mut document, &context).unwrap();

        assert_eq!(
            document.il.pages[0].paragraphs[1].preserved,
            Some(il::PreservedReason::TypesetProtocol)
        );
        assert_eq!(document.il.pages[0].paragraphs[2].preserved, None);
        let values = document.rewrites[0]
            .typeset_characters
            .iter()
            .map(|character| character.unicode)
            .collect::<String>();
        assert_eq!(values, "测试");
    }

    #[test]
    fn preserved_shared_operand_preserves_only_its_span_component() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("preserved-shared-span.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\n0 -20 Td (MIMUS) Tj\nET\n".to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();
        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        chars.sort_by(|left, right| {
            right
                .baseline_origin
                .y
                .total_cmp(&left.baseline_origin.y)
                .then_with(|| left.baseline_origin.x.total_cmp(&right.baseline_origin.x))
        });
        let bounds = Rect {
            left: 70.0,
            bottom: 90.0,
            right: 170.0,
            top: 135.0,
        };
        for character in &mut chars {
            character.layout.as_mut().unwrap().bounds = bounds;
        }
        document.extracted_pages[0].layout_regions.clear();
        assert_eq!(span_key(&chars[0], (9, 0)), span_key(&chars[4], (9, 0)));
        assert_ne!(span_key(&chars[0], (9, 0)), span_key(&chars[5], (9, 0)));
        document.il.pages[0].paragraphs = vec![
            Paragraph {
                reading_order: 0,
                bounds,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[..1].to_vec(),
                },
                translated_text: None,
                translation_conservation: None,
                preserved: Some(il::PreservedReason::UnreliableUnicode),
            },
            Paragraph {
                reading_order: 1,
                bounds,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[1..5].to_vec(),
                },
                translated_text: Some("中文".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
            Paragraph {
                reading_order: 2,
                bounds,
                first_line_indent: None,
                text: TextCarrier::Chars {
                    chars: chars[5..].to_vec(),
                },
                translated_text: Some("测试".to_owned()),
                translation_conservation: None,
                preserved: None,
            },
        ];

        typeset(&mut document, &context).unwrap();

        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::UnreliableUnicode)
        );
        assert_eq!(
            document.il.pages[0].paragraphs[1].preserved,
            Some(il::PreservedReason::TypesetProtocol)
        );
        assert_eq!(document.il.pages[0].paragraphs[2].preserved, None);
        let values = document.rewrites[0]
            .typeset_characters
            .iter()
            .map(|character| character.unicode)
            .collect::<String>();
        assert_eq!(values, "测试");
    }

    #[test]
    fn finite_pdfium_baseline_differences_become_one_page_alignment_diagnostic() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let walked = walk_page(&pdf, page_id).unwrap().characters;
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        engine[0].baseline_origin.x += 0.01;
        engine[0].baseline_origin.y -= 0.02;
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );
        assert_eq!(alignment.engine_indices_by_walk[0], None);
        assert!(
            diagnostics
                .entries()
                .iter()
                .all(|diagnostic| !matches!(diagnostic, Diagnostic::EngineBaselineMismatch { .. }))
        );
        assert_eq!(
            diagnostics
                .entries()
                .iter()
                .filter(|diagnostic| matches!(
                    diagnostic,
                    Diagnostic::EngineCharacterAlignment { page_index: 0, .. }
                ))
                .count(),
            1
        );
        assert!(diagnostics.entries().iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::EngineCharacterAlignment {
                page_index: 0,
                baseline_residual_count: 1,
                baseline_residual_max_delta_x_pt,
                baseline_residual_max_delta_y_pt,
                ..
            } if (*baseline_residual_max_delta_x_pt - 0.01).abs() < 1e-12
                && (*baseline_residual_max_delta_y_pt - 0.02).abs() < 1e-12
        )));
        assert!(diagnostics.debug_events().iter().any(|diagnostic| matches!(
            diagnostic,
            crate::event::DiagnosticEvent::EngineBaselineMismatch {
                page_index: 0,
                character_index: 0,
                delta_x_pt,
                delta_y_pt,
            } if (delta_x_pt - 0.01).abs() < 1e-12
                && (delta_y_pt - 0.02).abs() < 1e-12
        )));

        engine[0].baseline_origin.x = f64::NAN;
        diagnostics = Diagnostics::default();
        validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );
        assert!(diagnostics.entries().iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::EngineCharacterAlignment {
                residual_count: 1,
                ..
            }
        )));
    }

    #[test]
    fn unicode_conflicts_are_classified_without_overriding_the_walk() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let walked = walk_page(&pdf, page_id).unwrap().characters;
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        engine[0].unicode = Some('X');
        engine[0].unicode_value = u32::from('X');
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );
        assert_eq!(alignment.engine_indices_by_walk[0], Some(0));
        assert!(alignment.weak_unicode_conflicts.is_empty());
        assert_eq!(walked[0].unicode, Some('M'));
        assert!(matches!(
            diagnostics.entries(),
            [Diagnostic::EngineCharacterAlignment {
                page_index: 0,
                walked_character_count: 5,
                engine_character_count: 5,
                extraction_equivalent_count: 0,
                explained_count: 0,
                strong_unicode_conflict_count: 1,
                weak_unicode_conflict_count: 0,
                unresolved_unicode_count: 0,
                walk_only_count: 0,
                engine_only_count: 0,
                residual_count: 0,
                baseline_residual_count: 0,
                baseline_residual_max_delta_x_pt: 0.0,
                baseline_residual_max_delta_y_pt: 0.0,
            }]
        ));
    }

    #[test]
    fn independent_space_shows_are_extraction_equivalent() {
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(A) Tj [( )] TJ\nET\n".to_vec());
        let cmap = pdf
            .get_object((8, 0))
            .unwrap()
            .as_stream()
            .unwrap()
            .decompressed_content()
            .unwrap();
        let cmap = String::from_utf8(cmap)
            .unwrap()
            .replace("4 beginbfchar", "6 beginbfchar\n<20> <0020>\n<41> <0041>");
        pdf.get_object_mut((8, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(cmap.into_bytes());
        let page_id = pdf.get_pages()[&1];
        let mut walked = walk_page(&pdf, page_id).unwrap().characters;
        walked[0].advance = 1.0;
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        engine.truncate(1);
        engine[0].unicode = Some('A');
        engine[0].unicode_value = u32::from('A');
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );

        assert_eq!(walked.len(), 2);
        assert_eq!(
            walked
                .iter()
                .map(|character| character.unicode)
                .collect::<Vec<_>>(),
            [Some('A'), Some(' ')]
        );
        assert_eq!(engine.len(), 1);
        assert_eq!(alignment.engine_indices_by_walk, [Some(0), None]);
        assert!(alignment.weak_unicode_conflicts.is_empty());
        assert!(diagnostics.entries().is_empty());
    }

    #[test]
    fn baseline_matching_preserves_multiplicity_and_ignores_array_order() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let walked = walk_page(&pdf, page_id).unwrap().characters;
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        engine.reverse();
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );
        assert_eq!(
            alignment.engine_indices_by_walk,
            [Some(4), Some(3), Some(2), Some(1), Some(0)]
        );
        assert!(diagnostics.entries().is_empty());

        let doubled_walk = vec![walked[0].clone(), walked[0].clone()];
        let doubled_engine = vec![engine[4].clone(), engine[4].clone()];
        let doubled = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &doubled_walk,
            &doubled_engine,
            0.001,
            &mut diagnostics,
        );
        assert_eq!(
            doubled
                .engine_indices_by_walk
                .iter()
                .flatten()
                .copied()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0, 1])
        );
    }

    #[test]
    fn non_translatable_walk_characters_explain_unique_engine_observations() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut walked = walk_page(&pdf, page_id).unwrap().characters[..3].to_vec();
        walked[0].text_transform = TextTransform::Rotated(90.0);
        walked[1].unicode = None;
        walked[1].unicode_provenance = UnicodeProvenance::Unresolved;
        walked[2].advance = 0.0;
        let engine = FakeEngine::default().page_characters(&[], 0).unwrap()[..3].to_vec();
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );

        assert_eq!(alignment.engine_indices_by_walk, [None, None, None]);
        assert!(alignment.weak_unicode_conflicts.is_empty());
        assert_eq!(
            paragraph_preserved_reason(walked.iter().enumerate(), &BTreeSet::new(), false),
            Some(il::PreservedReason::UnreliableUnicode)
        );
        assert!(diagnostics.entries().is_empty());
    }

    #[test]
    fn ambiguous_explanation_candidates_preserve_multiplicity_as_residuals() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut walk_character = walk_page(&pdf, page_id).unwrap().characters[0].clone();
        walk_character.text_transform = TextTransform::Rotated(90.0);
        let walked = vec![walk_character.clone(), walk_character];
        let engine_character = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let engine = vec![engine_character.clone(), engine_character];
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );

        assert_eq!(alignment.engine_indices_by_walk, [None, None]);
        assert!(matches!(
            diagnostics.entries(),
            [Diagnostic::EngineCharacterAlignment {
                explained_count: 0,
                walk_only_count: 0,
                engine_only_count: 0,
                residual_count: 4,
                ..
            }]
        ));
    }

    #[test]
    fn extraction_marker_surrogate_and_ligature_pairs_are_equivalent() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let source = walk_page(&pdf, page_id).unwrap().characters;
        let mut walked = source[..3].to_vec();
        walked[0].unicode = Some('-');
        walked[1].unicode = Some('\u{1D11E}');
        walked[2].unicode = Some('\u{FB01}');
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap()[..3].to_vec();
        engine[0].unicode = Some('\u{2}');
        engine[0].unicode_value = 2;
        let mut utf16 = [0u16; 2];
        engine[1].unicode = None;
        engine[1].unicode_value = u32::from('\u{1D11E}'.encode_utf16(&mut utf16)[0]);
        engine[2].unicode = Some('f');
        engine[2].unicode_value = u32::from('f');
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );

        assert!(alignment.engine_indices_by_walk.iter().all(Option::is_some));
        assert!(diagnostics.entries().is_empty());
    }

    #[test]
    fn unmatched_characters_follow_the_transition_matrix_without_preservation() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let source = walk_page(&pdf, page_id).unwrap().characters;
        let mut whitespace = source[0].clone();
        whitespace.unicode = Some(' ');
        whitespace.baseline_origin.x = 10.0;
        let mut invisible = source[0].clone();
        invisible.visible = false;
        invisible.baseline_origin.x = 20.0;
        let mut walk_only = source[0].clone();
        walk_only.baseline_origin.x = 30.0;
        let mut residual_walk = source[0].clone();
        residual_walk.locatable = false;
        let walked = vec![whitespace, invisible, walk_only, residual_walk];

        let source_engine = FakeEngine::default().page_characters(&[], 0).unwrap()[0].clone();
        let mut outside = source_engine.clone();
        outside.baseline_origin.x = 400.0;
        outside.tight_box = Rect {
            left: 400.0,
            bottom: 10.0,
            right: 410.0,
            top: 20.0,
        };
        let mut engine_only = source_engine.clone();
        engine_only.baseline_origin.x = 50.0;
        let mut residual_engine = source_engine;
        residual_engine.baseline_origin.x = f64::NAN;
        let engine = vec![outside, engine_only, residual_engine];
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            0.001,
            &mut diagnostics,
        );

        assert!(alignment.weak_unicode_conflicts.is_empty());
        assert!(
            diagnostics.entries().iter().any(|diagnostic| matches!(
                diagnostic,
                Diagnostic::EngineCharacterAlignment {
                    extraction_equivalent_count: 3,
                    walk_only_count: 1,
                    engine_only_count: 1,
                    residual_count: 2,
                    ..
                }
            )),
            "{:?}",
            diagnostics.entries()
        );
    }

    #[test]
    fn weak_unicode_conflicts_preserve_the_owning_paragraph() {
        for provenance in [
            UnicodeProvenance::SimpleEncoding,
            UnicodeProvenance::DifferencesAgl,
        ] {
            let pdf = LopdfDocument::load(fixture()).unwrap();
            let page_id = pdf.get_pages()[&1];
            let mut walked = walk_page(&pdf, page_id).unwrap().characters;
            walked[0].unicode_provenance = provenance;
            let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
            engine[0].unicode = Some('X');
            engine[0].unicode_value = u32::from('X');
            let mut diagnostics = Diagnostics::default();

            let alignment = validate_character_alignment(
                0,
                FakeEngine::default().page_geometry(&[], 0).unwrap(),
                &walked,
                &engine,
                0.001,
                &mut diagnostics,
            );

            assert_eq!(alignment.weak_unicode_conflicts, BTreeSet::from([0]));
            assert_eq!(
                paragraph_preserved_reason(
                    walked.iter().enumerate(),
                    &alignment.weak_unicode_conflicts,
                    false,
                ),
                Some(il::PreservedReason::UnreliableUnicode)
            );
            assert!(matches!(
                diagnostics.entries(),
                [Diagnostic::EngineCharacterAlignment {
                    weak_unicode_conflict_count: 1,
                    ..
                }]
            ));
        }
    }

    #[test]
    fn paragraph_find_uses_per_character_boxes_but_never_pdfium_unicode() {
        let mut engine_characters = FakeEngine::default().page_characters(&[], 0).unwrap();
        let expected_first_box = engine_characters[0].tight_box;
        engine_characters[0].unicode = Some('X');
        engine_characters[0].unicode_value = u32::from('X');
        engine_characters.remove(1);
        engine_characters.reverse();
        let engine = FakeEngine {
            characters: Some(engine_characters),
            ..FakeEngine::default()
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
        let mut document = Document::new(fixture(), "unused.pdf");
        for pass in [parse as Pass, scan_detect, layout, paragraph_find] {
            pass(&mut document, &context).unwrap();
        }

        let paragraph = &document.il.pages[0].paragraphs[0];
        assert_eq!(paragraph.source_text(), "MIMUS");
        assert_eq!(paragraph.chars()[0].unicode, Some('M'));
        assert_eq!(paragraph.chars()[0].visual_bbox, expected_first_box);
        assert_eq!(paragraph.chars()[1].visual_bbox, paragraph.chars()[1].r#box);
        assert_eq!(paragraph.preserved, None);
    }

    #[test]
    fn invisible_characters_do_not_join_visible_paragraphs_or_translation_requests() {
        let engine = FakeEngine::default();
        let translator = WrappingTranslator::default();
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
        for pass in [parse as Pass, scan_detect, layout] {
            pass(&mut document, &context).unwrap();
        }
        let mut invisible = document.extracted_pages[0].walked_characters[0].clone();
        invisible.unicode = Some('X');
        invisible.visible = false;
        invisible.baseline_origin.x += 2.0;
        invisible.metric_box.left += 2.0;
        invisible.metric_box.right += 2.0;
        document.extracted_pages[0]
            .walked_characters
            .push(invisible);

        paragraph_find(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();

        let visible = document.il.pages[0]
            .paragraphs
            .iter()
            .find(|paragraph| paragraph.source_text() == "MIMUS")
            .unwrap();
        let invisible = document.il.pages[0]
            .paragraphs
            .iter()
            .find(|paragraph| paragraph.source_text() == "X")
            .unwrap();
        assert_eq!(translator.inputs.lock().unwrap().as_slice(), ["MIMUS"]);
        assert_eq!(visible.translated_text.as_deref(), Some("[MIMUS]"));
        assert_eq!(invisible.translated_text.as_deref(), Some("X"));
    }

    #[test]
    fn to_unicode_noncharacters_preserve_the_paragraph_and_exact_input_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let input_path = directory.path().join("noncharacter.pdf");
        let output_path = directory.path().join("noncharacter-output.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        let cmap = pdf
            .get_object((8, 0))
            .unwrap()
            .as_stream()
            .unwrap()
            .decompressed_content()
            .unwrap();
        let cmap = String::from_utf8(cmap)
            .unwrap()
            .replace("<4D> <004D>", "<4D> <FFFF>");
        pdf.get_object_mut((8, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(cmap.into_bytes());
        pdf.save(&input_path).unwrap();
        let input = std::fs::read(&input_path).unwrap();
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let mut document = Document::new(&input_path, &output_path);
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &NonIdentityTranslator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };

        run(&mut document, &context).unwrap();

        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::UnreliableUnicode)
        );
        assert!(document.il.pages[0].paragraphs[0].translated_text.is_none());
        assert!(document.rewrites.is_empty());
        assert_eq!(std::fs::read(&output_path).unwrap(), input);
        assert!(
            !input
                .windows(b"(cid:".len())
                .any(|window| window == b"(cid:")
        );
        assert!(
            document.extracted_pages[0]
                .walked_characters
                .iter()
                .filter(|character| character.code == u32::from(b'M'))
                .all(|character| {
                    character.unicode.is_none()
                        && character.unicode_provenance == UnicodeProvenance::Unresolved
                })
        );
    }

    #[test]
    fn invalid_classifier_tolerance_uses_the_existing_fallback_diagnostic() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let walked = walk_page(&pdf, page_id).unwrap().characters;
        let engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        let mut diagnostics = Diagnostics::default();

        let alignment = validate_character_alignment(
            0,
            FakeEngine::default().page_geometry(&[], 0).unwrap(),
            &walked,
            &engine,
            f64::NAN,
            &mut diagnostics,
        );

        assert!(alignment.engine_indices_by_walk.iter().all(Option::is_none));
        assert!(matches!(
            diagnostics.entries(),
            [Diagnostic::EngineCharacterMismatch {
                character_index: None,
                walked_character_count: 5,
                engine_character_count: 5,
                ..
            }]
        ));
    }

    #[test]
    fn form_origin_characters_are_typed_and_remain_passthrough_in_the_full_pipeline() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("form-output.pdf");
        let mut document = Document::new(form_fixture(), &output);
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

        assert_eq!(document.il.pages[0].paragraphs[0].source_text(), "MIMUS");
        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::FormXObjectContent)
        );
        assert!(document.il.pages[0].paragraphs[0].translated_text.is_none());
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert!(document.rewrites.is_empty());
        assert_eq!(
            std::fs::read(&output).unwrap(),
            std::fs::read(form_fixture()).unwrap()
        );
    }

    #[test]
    fn mixed_page_and_form_translation_content_keeps_the_page_owned_unit_processable() {
        let mut document = Document::for_inspection(form_fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        for pass in [parse as Pass, scan_detect, layout] {
            pass(&mut document, &context).unwrap();
        }

        let page_content_object = document.extracted_pages[0].content_streams[0].object_id;
        let mut page_owned = document.extracted_pages[0].walked_characters.clone();
        for character in &mut page_owned {
            character.content_object = page_content_object;
        }
        document.extracted_pages[0]
            .walked_characters
            .extend(page_owned);

        paragraph_find(&mut document, &context).unwrap();

        let mixed =
            document.il.pages[0]
                .paragraphs
                .iter()
                .find(|paragraph| {
                    paragraph.chars().iter().any(|character| {
                        character.passthrough.content_object == page_content_object.0
                    }) && paragraph.chars().iter().any(|character| {
                        character.passthrough.content_object != page_content_object.0
                    })
                })
                .expect("the overlapping page and Form text should share a paragraph");
        assert_eq!(mixed.preserved, None);
    }

    #[test]
    fn translate_only_sends_visible_upright_page_content_units() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = WrappingTranslator::default();
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

        let content_object = document.extracted_pages[0].content_streams[0].object_id.0;
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        chars[2].text_transform = TextTransform::Rotated(90.0);
        chars[3].visible = false;
        chars[4].passthrough.content_object = u32::MAX;
        let mut trailing = chars[4].clone();
        trailing.unicode = Some('!');
        trailing.code = u32::from(b'!');
        trailing.visible = true;
        trailing.text_transform = TextTransform::Upright;
        trailing.passthrough.content_object = content_object;
        trailing.passthrough.encoded = vec![b'!'];
        chars.push(trailing);

        translate(&mut document, &context).unwrap();

        assert_eq!(translator.inputs.lock().unwrap().as_slice(), ["MI!"]);
        assert_eq!(
            document.il.pages[0].paragraphs[0]
                .translated_text
                .as_deref(),
            Some("[MI!]")
        );
    }

    #[test]
    fn math_shape_heuristic_prefers_source_without_hiding_plain_prose() {
        for math in [
            "(1)",
            "Q",
            "dmodel×dk",
            "Attention(Q,K,V) = softmax(QK^T / sqrt(dk))V (1)",
            "MultiHead(Q,K,V) = Concat(head1,...,headh)WO",
            "PE(pos,2i+1) = cos(pos/10000)",
            "x² + y₁",
        ] {
            assert!(math_shape_is_passthrough(math), "missed {math:?}");
        }
        for prose in [
            "M",
            "I",
            "This method improves translation quality across documents.",
            "Attention Is All You Need",
            "Figure 1: Overview",
            "In 2024, we measured 3.14 and reported the result.",
        ] {
            assert!(!math_shape_is_passthrough(prose), "matched {prose:?}");
        }
    }

    #[test]
    fn math_shape_heuristic_accepts_a_whole_model_math_paragraph_only() {
        for math in ["dmodel×dk", "x = y"] {
            assert!(
                model_math_shape_is_passthrough(math),
                "missed model formula {math:?}"
            );
        }
        for prose in [
            "Footnote marker appears inside word¹ without starting a new paragraph.",
            "x = y describes the observed relation in prose.",
        ] {
            assert!(
                !model_math_shape_is_passthrough(prose),
                "matched model prose {prose:?}"
            );
        }

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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars.truncate(3);
        for (character, unicode) in chars.iter_mut().zip(['x', '=', 'y']) {
            character.unicode = Some(unicode);
            let layout = character.layout.as_mut().unwrap();
            layout.label = LayoutLabel::Text;
            layout.source = LayoutSource::Model;
            layout.policy = TranslationPolicy::Translate;
        }
        let content_objects = BTreeSet::from([chars[0].passthrough.content_object]);
        let mut diagnostics = Vec::new();

        mark_math_passthrough_units(paragraph, &content_objects, 0, 0, &mut diagnostics);

        assert_eq!(diagnostics.len(), 1);
        assert!(paragraph.chars().iter().all(|character| {
            character.layout.unwrap().policy == TranslationPolicy::Passthrough
        }));

        let TextCarrier::Chars { chars } = &mut paragraph.text;
        for (character, unicode) in chars.iter_mut().zip(['M', 'I', 'M']) {
            character.unicode = Some(unicode);
            character.layout.as_mut().unwrap().policy = TranslationPolicy::Translate;
        }
        diagnostics.clear();

        mark_math_passthrough_units(paragraph, &content_objects, 0, 0, &mut diagnostics);

        assert!(diagnostics.is_empty());
        assert!(
            paragraph.chars().iter().all(|character| {
                character.layout.unwrap().policy == TranslationPolicy::Translate
            })
        );
    }

    #[test]
    fn formula_boundary_completion_requires_a_model_anchor_and_no_word_break() {
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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        for character in chars.iter_mut() {
            let layout = character.layout.as_mut().unwrap();
            layout.label = LayoutLabel::Text;
            layout.source = LayoutSource::Model;
            layout.policy = TranslationPolicy::Translate;
        }
        let content_objects = BTreeSet::from([chars[0].passthrough.content_object]);
        let mut diagnostics = Vec::new();

        complete_model_formula_boundaries(paragraph, &content_objects, 0, 0, &mut diagnostics);
        assert!(diagnostics.is_empty());
        assert!(
            paragraph
                .chars()
                .iter()
                .all(|character| { character.layout.unwrap().label == LayoutLabel::Text })
        );

        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars[0].layout.as_mut().unwrap().label = LayoutLabel::InlineFormula;
        chars[0].layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        chars[1].implicit_space_before = true;
        complete_model_formula_boundaries(paragraph, &content_objects, 0, 0, &mut diagnostics);

        assert!(diagnostics.is_empty());
        assert_eq!(
            paragraph.chars()[1].layout.unwrap().label,
            LayoutLabel::Text
        );

        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars[1].implicit_space_before = false;
        complete_model_formula_boundaries(paragraph, &content_objects, 0, 0, &mut diagnostics);

        assert!(diagnostics.is_empty());
        assert_eq!(
            paragraph.chars()[1].layout.unwrap().label,
            LayoutLabel::Text,
            "an ASCII formula anchor does not prove that a shared prose font is mathematical"
        );
    }

    #[test]
    fn formula_boundary_attaches_a_unique_geometric_superscript_across_extraction_order() {
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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars.truncate(5);

        // The real (6,1) shape differs from an adjacent script run: PDF extraction
        // places the raised `2` before intervening prose even though it is visually
        // attached to the model-owned final `d` at the end of the formula.
        for (index, character) in chars.iter_mut().enumerate() {
            character.unicode = Some(['x', '2', 'p', 'q', 'd'][index]);
            character.implicit_space_before = index == 1;
            character.font_size = 10.0;
            character.baseline_origin = Point {
                x: 100.0 + index as f64 * 20.0,
                y: 100.0,
            };
            character.r#box = Rect {
                left: character.baseline_origin.x,
                bottom: 98.0,
                right: character.baseline_origin.x + 6.0,
                top: 108.0,
            };
            character.visual_bbox = character.r#box;
            let layout = character.layout.as_mut().unwrap();
            layout.source = LayoutSource::Model;
            layout.label = LayoutLabel::Text;
            layout.policy = TranslationPolicy::Translate;
        }
        chars[4].layout.as_mut().unwrap().label = LayoutLabel::InlineFormula;
        chars[4].layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        chars[4].layout.as_mut().unwrap().reading_order = 114;
        chars[4].r#box = Rect {
            left: 246.404,
            bottom: 582.625,
            right: 251.590,
            top: 591.472,
        };
        chars[4].visual_bbox = chars[4].r#box;
        chars[4].baseline_origin = Point {
            x: 246.404,
            y: 584.558,
        };

        chars[1].font_size = 6.974;
        chars[1].r#box = Rect {
            left: 251.590,
            bottom: 586.820,
            right: 255.562,
            top: 593.013,
        };
        chars[1].visual_bbox = chars[1].r#box;
        chars[1].baseline_origin = Point {
            x: 251.590,
            y: 588.173,
        };

        let content_objects = BTreeSet::from([chars[0].passthrough.content_object]);
        let cross_order = paragraph.clone();
        let mut diagnostics = Vec::new();
        complete_model_formula_boundaries(paragraph, &content_objects, 6, 1, &mut diagnostics);

        assert_eq!(
            paragraph
                .chars()
                .iter()
                .filter_map(|character| character.unicode)
                .collect::<String>(),
            "xpqd2"
        );
        assert_eq!(
            paragraph.chars()[4].layout.unwrap(),
            LayoutAssignment {
                label: LayoutLabel::InlineFormula,
                policy: TranslationPolicy::Passthrough,
                reading_order: 114,
                ..paragraph.chars()[4].layout.unwrap()
            }
        );
        assert!(
            paragraph.chars()[0..3]
                .iter()
                .all(|character| character.layout.unwrap().label == LayoutLabel::Text)
        );
        assert_eq!(
            paragraph.chars()[3].layout.unwrap().label,
            LayoutLabel::InlineFormula
        );
        assert!(diagnostics.iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::FormulaBoundaryExpanded {
                evidence: FormulaBoundaryEvidence::ScriptBaseline,
                expanded_character_count: 1,
                ..
            }
        )));

        *paragraph = cross_order.clone();
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars[1].unicode = Some('d');
        diagnostics.clear();

        complete_model_formula_boundaries(paragraph, &content_objects, 6, 1, &mut diagnostics);

        assert_eq!(
            paragraph.chars()[1].layout.unwrap().label,
            LayoutLabel::Text,
            "cross-order alphabetic scripts remain ambiguous with prose"
        );
        assert!(diagnostics.is_empty());

        *paragraph = cross_order;
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars[3].layout.as_mut().unwrap().label = LayoutLabel::InlineFormula;
        chars[3].layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        chars[3].layout.as_mut().unwrap().reading_order = 115;
        chars[3].r#box = chars[4].r#box;
        chars[3].visual_bbox = chars[4].visual_bbox;
        chars[3].baseline_origin = chars[4].baseline_origin;
        diagnostics.clear();

        complete_model_formula_boundaries(paragraph, &content_objects, 6, 1, &mut diagnostics);

        assert_eq!(
            paragraph.chars()[1].layout.unwrap().label,
            LayoutLabel::Text
        );
        assert!(
            diagnostics.is_empty(),
            "ambiguous geometry must fail closed"
        );
    }

    #[test]
    fn formula_boundary_reorders_a_signed_superscript_with_its_numeric_base() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars.truncate(5);
        for (index, character) in chars.iter_mut().enumerate() {
            character.unicode = Some(['−', '9', 'x', '1', '0'][index]);
            character.font_size = 9.9626;
            character.baseline_origin = Point {
                x: 100.0 + index as f64 * 10.0,
                y: 206.268,
            };
            character.r#box = Rect {
                left: character.baseline_origin.x,
                bottom: 204.0,
                right: character.baseline_origin.x + 5.0,
                top: 214.0,
            };
            character.visual_bbox = character.r#box;
            character.implicit_space_before = false;
            let layout = character.layout.as_mut().unwrap();
            layout.source = LayoutSource::Model;
            layout.label = LayoutLabel::Text;
            layout.policy = TranslationPolicy::Translate;
            layout.reading_order = 220;
        }
        chars[1].layout.as_mut().unwrap().label = LayoutLabel::InlineFormula;
        chars[1].layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        chars[1].layout.as_mut().unwrap().reading_order = 240;
        for character in chars.iter_mut().take(2) {
            character.font_size = 6.9738;
            character.baseline_origin.y = 209.883;
        }
        chars[0].r#box = Rect {
            left: 396.409,
            bottom: 208.0,
            right: 402.636,
            top: 214.0,
        };
        chars[0].visual_bbox = chars[0].r#box;
        chars[1].r#box = Rect {
            left: 402.636,
            bottom: 208.0,
            right: 406.607,
            top: 214.0,
        };
        chars[1].visual_bbox = chars[1].r#box;
        chars[3].r#box = Rect {
            left: 386.449,
            bottom: 204.0,
            right: 391.430,
            top: 214.0,
        };
        chars[3].visual_bbox = chars[3].r#box;
        chars[4].r#box = Rect {
            left: 391.430,
            bottom: 204.0,
            right: 396.411,
            top: 214.0,
        };
        chars[4].visual_bbox = chars[4].r#box;

        let content_objects = BTreeSet::from([chars[0].passthrough.content_object]);
        let mut diagnostics = Vec::new();
        complete_model_formula_boundaries(paragraph, &content_objects, 6, 10, &mut diagnostics);

        assert_eq!(
            paragraph
                .chars()
                .iter()
                .filter_map(|character| character.unicode)
                .collect::<String>(),
            "x10−9"
        );
        assert_eq!(
            paragraph.chars()[0].layout.unwrap().label,
            LayoutLabel::Text
        );
        assert!(
            paragraph.chars()[1..]
                .iter()
                .all(|character| character.layout.unwrap().label == LayoutLabel::InlineFormula)
        );
        assert!(diagnostics.iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::FormulaBoundaryExpanded {
                evidence: FormulaBoundaryEvidence::ScriptBaseline,
                expanded_character_count: 2,
                ..
            }
        )));
    }

    #[test]
    fn nested_inline_formula_group_joins_its_translatable_model_owner() {
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
        let source = document.il.pages[0].paragraphs[0].chars();
        let text_assignment = LayoutAssignment {
            label: LayoutLabel::Text,
            reading_order: 10,
            bounds: Rect {
                left: 60.0,
                bottom: 100.0,
                right: 140.0,
                top: 140.0,
            },
            source: LayoutSource::Model,
            policy: TranslationPolicy::Translate,
        };
        let formula_assignment = LayoutAssignment {
            label: LayoutLabel::InlineFormula,
            reading_order: 11,
            bounds: Rect {
                left: 82.0,
                bottom: 115.0,
                right: 94.0,
                top: 132.0,
            },
            source: LayoutSource::Model,
            policy: TranslationPolicy::Passthrough,
        };
        let positioned = |index: usize, assignment| {
            let mut character = source[index].clone();
            character.layout = Some(assignment);
            PositionedChar {
                walked_index: index,
                locatable: true,
                character,
                force_no_space_before: false,
                small_edge_character: false,
            }
        };
        let mut groups = vec![
            ModelGroup {
                assignment: text_assignment,
                chars: [0, 1, 3, 4]
                    .into_iter()
                    .map(|index| positioned(index, text_assignment))
                    .collect(),
            },
            ModelGroup {
                assignment: formula_assignment,
                chars: vec![positioned(2, formula_assignment)],
            },
        ];

        merge_nested_inline_formula_groups(&mut groups);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].chars.len(), 5);
        assert_eq!(
            groups[0]
                .chars
                .iter()
                .filter(|positioned| positioned.character.layout.unwrap().label
                    == LayoutLabel::InlineFormula)
                .count(),
            1
        );
    }

    #[test]
    fn inline_formula_uses_one_placeholder_and_its_source_span_is_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("inline-formula.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 120 Tm\n(MI) Tj (M) Tj (US) Tj\nET\n".to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "\u{4e2d}{v1}\u{6587}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        for character in &mut chars[..2] {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 90.0,
                right: 122.0,
                top: 135.0,
            };
        }
        for character in &mut chars[3..] {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 97.0,
                bottom: 90.0,
                right: 180.0,
                top: 135.0,
            };
        }
        let formula_span = (
            chars[2].passthrough.content_object,
            chars[2].passthrough.byte_start,
            chars[2].passthrough.byte_end,
        );
        let layout = chars[2].layout.as_mut().unwrap();
        layout.label = LayoutLabel::InlineFormula;
        layout.policy = TranslationPolicy::Passthrough;

        styles_and_formulas(&mut document, &context).unwrap();
        assert_eq!(
            document
                .prepared_translations
                .get(&(0, 0))
                .unwrap()
                .request_text(),
            "MI{v1}US"
        );
        translate(&mut document, &context).unwrap();
        assert_eq!(
            document.il.pages[0].paragraphs[0]
                .translated_text
                .as_deref(),
            Some("\u{4e2d}\u{6587}")
        );

        typeset(&mut document, &context).unwrap();
        assert!(
            document.il.pages[0].paragraphs[0].preserved.is_none(),
            "{:?}",
            document.il.pages[0].paragraphs[0].preserved
        );
        assert_eq!(document.rewrites.len(), 1);
        assert!(document.rewrites[0].replacements.len() >= 2);
        assert!(document.rewrites.iter().all(|rewrite| {
            rewrite.replacements.iter().all(|replacement| {
                (
                    replacement.content_object.0,
                    replacement.byte_start,
                    replacement.byte_end,
                ) != formula_span
            })
        }));
    }

    #[test]
    fn mixed_formula_text_that_cannot_fit_its_own_slots_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("mixed-formula-slots.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 120 Tm\n(M) Tj (I) Tj (M) Tj (U) Tj (S) Tj\nET\n"
                    .to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "\u{6a21}\u{578b}\u{6570}\u{636e}\u{9a8c}\u{8bc1}{v1}\u{7ffb}\u{8bd1}\u{7ed3}\u{679c}\u{4fdd}\u{6301}{v2}\u{7ed3}\u{6784}\u{6d41}\u{7a0b}\u{7a33}\u{5b9a}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        assert_eq!(chars.len(), 5);
        for character in chars.iter_mut() {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 90.0,
                right: 260.0,
                top: 135.0,
            };
        }
        document.extracted_pages[0].layout_regions[0].bounds = Rect {
            left: 72.0,
            bottom: 90.0,
            right: 260.0,
            top: 135.0,
        };
        for index in [1, 3] {
            let layout = chars[index].layout.as_mut().unwrap();
            layout.label = LayoutLabel::InlineFormula;
            layout.policy = TranslationPolicy::Passthrough;
        }

        styles_and_formulas(&mut document, &context).unwrap();
        assert_eq!(
            document
                .prepared_translations
                .values()
                .next()
                .unwrap()
                .request_text(),
            "M{v1}M{v2}S"
        );
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::TypesetOverflow)
        );
        assert!(document.rewrites.is_empty());
    }

    #[test]
    fn multiline_mixed_formula_text_flows_with_relocated_source_operands() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("multiline-mixed-formula-flow.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 140 Tm\n(M) Tj (I) Tj (M) Tj (U) Tj (S) Tj\n1 0 0 1 72 126 Tm\n(M) Tj (I) Tj (M) Tj (U) Tj (S) Tj\nET\n"
                    .to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "\u{6a21}\u{578b}\u{6570}\u{636e}\u{9a8c}\u{8bc1}{v1}\u{7ffb}\u{8bd1}\u{7ed3}\u{679c}\u{4fdd}\u{6301}{v2}\u{7ed3}\u{6784}\u{6d41}\u{7a0b}\u{7a33}\u{5b9a}{v3}\u{7f13}\u{5b58}\u{91cd}\u{8bd5}\u{8bca}\u{65ad}\u{6392}\u{7248}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs.len(), 1);
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        assert_eq!(chars.len(), 10);
        for character in chars.iter_mut() {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 110.0,
                right: 260.0,
                top: 151.0,
            };
        }
        document.extracted_pages[0].layout_regions[0].bounds = Rect {
            left: 72.0,
            bottom: 110.0,
            right: 260.0,
            top: 151.0,
        };
        let formula_indices = [1, 3, 6];
        let formula_spans = formula_indices
            .iter()
            .map(|index| {
                let character = &mut chars[*index];
                let layout = character.layout.as_mut().unwrap();
                layout.label = LayoutLabel::InlineFormula;
                layout.policy = TranslationPolicy::Passthrough;
                (
                    (character.passthrough.content_object, 0),
                    character.passthrough.byte_start,
                    character.passthrough.byte_end,
                )
            })
            .collect::<Vec<_>>();

        styles_and_formulas(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        assert_eq!(document.rewrites.len(), 1);
        assert_typeset_ink_is_disjoint(&document.rewrites[0].typeset_ink_bounds, &[]);
        assert_eq!(
            document.rewrites[0]
                .typeset_characters
                .iter()
                .filter(|character| character.unicode.is_ascii_uppercase())
                .map(|character| character.unicode)
                .collect::<String>(),
            "IUI"
        );
        for span in formula_spans {
            let replacement = document.rewrites[0]
                .replacements
                .iter()
                .find(|replacement| {
                    (
                        replacement.content_object,
                        replacement.byte_start,
                        replacement.byte_end,
                    ) == span
                })
                .expect("formula operand is relocated in its own span");
            let source = document.extracted_pages[0]
                .content_streams
                .iter()
                .find(|stream| stream.object_id == span.0)
                .unwrap()
                .decoded
                .get(span.1..span.2)
                .unwrap();
            assert!(
                replacement
                    .replacement
                    .windows(source.len())
                    .any(|window| window == source),
                "formula operand bytes were not replayed"
            );
        }
    }

    #[test]
    fn distant_fixed_formula_slot_falls_back_to_relocation() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("distant-fixed-formula-slot.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 140 Tm\n(MIM) Tj\n1 0 0 1 220 140 Tm\n(U) Tj\n1 0 0 1 230 140 Tm\n(S) Tj\n1 0 0 1 72 126 Tm\n(MIMUS) Tj\nET\n"
                    .to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "\u{4e2d}{v1}\u{6587}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        chars.sort_by(|left, right| {
            right
                .baseline_origin
                .y
                .total_cmp(&left.baseline_origin.y)
                .then_with(|| left.baseline_origin.x.total_cmp(&right.baseline_origin.x))
        });
        assert_eq!(chars.len(), 10);
        let owner = Rect {
            left: 72.0,
            bottom: 110.0,
            right: 260.0,
            top: 151.0,
        };
        for character in chars.iter_mut() {
            character.layout.as_mut().unwrap().bounds = owner;
        }
        document.extracted_pages[0].layout_regions[0].bounds = owner;
        let formula = chars
            .iter_mut()
            .find(|character| character.unicode == Some('U') && character.baseline_origin.x > 200.0)
            .unwrap();
        let formula_span = (
            (formula.passthrough.content_object, 0),
            formula.passthrough.byte_start,
            formula.passthrough.byte_end,
        );
        let source_formula_x = formula.baseline_origin.x;
        let layout = formula.layout.as_mut().unwrap();
        layout.label = LayoutLabel::InlineFormula;
        layout.policy = TranslationPolicy::Passthrough;
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: owner,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: None,
            translation_conservation: None,
            preserved: None,
        }];

        styles_and_formulas(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        let replacement = document.rewrites[0]
            .replacements
            .iter()
            .find(|replacement| {
                (
                    replacement.content_object,
                    replacement.byte_start,
                    replacement.byte_end,
                ) == formula_span
            })
            .expect("continuity rejection must relocate the formula operand");
        assert!(
            replacement
                .replacement
                .windows(3)
                .any(|window| window == b"(U)"),
            "source formula bytes must be replayed"
        );
        let relocated_x = document.rewrites[0]
            .typeset_characters
            .iter()
            .find(|character| character.unicode == 'U')
            .unwrap()
            .baseline_origin
            .x;
        assert!(relocated_x + 50.0 < source_formula_x);
    }

    #[test]
    fn numbered_multiline_heading_relocates_formula_and_retains_source_gap() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("numbered-formula-heading.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 140 Tm\n[(M) -500 (IMUS)] TJ\n1 0 0 1 220 140 Tm\n(U) Tj\n1 0 0 1 230 140 Tm\n(S) Tj\n1 0 0 1 72 126 Tm\n(MIMUS) Tj\nET\n"
                    .to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "<b1>\u{53cd}\u{5411}\u{8fc7}\u{7a0b}</b1>{v1}<b2>\u{6269}\u{6563}\u{6a21}\u{578b}</b2>",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        chars.sort_by(|left, right| {
            right
                .baseline_origin
                .y
                .total_cmp(&left.baseline_origin.y)
                .then_with(|| left.baseline_origin.x.total_cmp(&right.baseline_origin.x))
        });
        let owner = Rect {
            left: 72.0,
            bottom: 110.0,
            right: 280.0,
            top: 152.0,
        };
        for character in &mut chars {
            let layout = character.layout.as_mut().unwrap();
            layout.bounds = owner;
            layout.label = LayoutLabel::ParagraphTitle;
            layout.policy = TranslationPolicy::Translate;
        }
        document.extracted_pages[0].layout_regions[0].bounds = owner;

        let number_index = chars
            .iter()
            .position(|character| {
                character.unicode == Some('M') && character.baseline_origin.y > 130.0
            })
            .unwrap();
        let title_index = chars
            .iter()
            .position(|character| {
                character.unicode == Some('I') && character.baseline_origin.y > 130.0
            })
            .unwrap();
        let number_span = (
            chars[number_index].passthrough.content_object,
            chars[number_index].passthrough.byte_start,
            chars[number_index].passthrough.byte_end,
        );
        let title_span = (
            chars[title_index].passthrough.content_object,
            chars[title_index].passthrough.byte_start,
            chars[title_index].passthrough.byte_end,
        );
        assert_eq!(
            number_span, title_span,
            "the number shares the title operand"
        );
        let source_prefix_left = chars[number_index].r#box.left;
        let source_title_left = chars[title_index].r#box.left;
        chars[number_index].unicode = Some('3');
        let number_layout = chars[number_index].layout.as_mut().unwrap();
        number_layout.label = LayoutLabel::Number;
        number_layout.policy = TranslationPolicy::Passthrough;

        let formula_index = chars
            .iter()
            .position(|character| {
                character.unicode == Some('U') && character.baseline_origin.x > 200.0
            })
            .unwrap();
        let formula_span = (
            (chars[formula_index].passthrough.content_object, 0),
            chars[formula_index].passthrough.byte_start,
            chars[formula_index].passthrough.byte_end,
        );
        let source_formula_x = chars[formula_index].baseline_origin.x;
        let formula_layout = chars[formula_index].layout.as_mut().unwrap();
        formula_layout.label = LayoutLabel::InlineFormula;
        formula_layout.policy = TranslationPolicy::Passthrough;
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: owner,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: None,
            translation_conservation: None,
            preserved: None,
        }];

        styles_and_formulas(&mut document, &context).unwrap();
        let request_text = document
            .prepared_translations
            .values()
            .next()
            .unwrap()
            .request_text();
        assert!(!request_text.contains('3'));
        assert!(request_text.contains("{v1}"));
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        let rewrite = &document.rewrites[0];
        let output_prefix = rewrite
            .typeset_characters
            .iter()
            .find(|character| character.unicode == '3')
            .unwrap();
        let output_title = rewrite
            .typeset_characters
            .iter()
            .find(|character| character.unicode == '\u{53cd}')
            .unwrap();
        let evidence = document.il.publication_ink[0]
            .section_number_gap
            .expect("relocated formula flow publishes its section-number geometry");
        assert!((output_prefix.baseline_origin.x - source_prefix_left).abs() <= 0.001);
        assert!(
            (output_title.baseline_origin.x - source_title_left).abs() <= 0.01,
            "output title x {} does not match source title x {}; evidence={evidence:?}",
            output_title.baseline_origin.x,
            source_title_left
        );
        assert!(evidence.prefix_in_output);
        assert!(!evidence.clamped);
        assert!((evidence.output_title_left - output_title.baseline_origin.x).abs() <= 0.001);

        let formula_replacement = rewrite
            .replacements
            .iter()
            .find(|replacement| {
                (
                    replacement.content_object,
                    replacement.byte_start,
                    replacement.byte_end,
                ) == formula_span
            })
            .expect("the distant formula is relocated");
        assert!(
            formula_replacement
                .replacement
                .windows(3)
                .any(|window| window == b"(U)")
        );
        let relocated_formula_x = rewrite
            .typeset_characters
            .iter()
            .find(|character| character.unicode == 'U')
            .unwrap()
            .baseline_origin
            .x;
        assert!(relocated_formula_x + 50.0 < source_formula_x);
    }

    #[test]
    fn relocated_formula_moves_its_uniquely_owned_vector_rule() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("vector-formula-relocation.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        const RULE: &[u8] = b"q\n1 0 0 1 218 148 cm\n0 0 m 16 0 l S\nQ";
        const INLINE_IMAGE: &[u8] =
            b"q\n12 0 0 12 220 136 cm\nBI /W 1 /H 1 /BPC 1 /CS /G ID\n\x80\nEI\nQ";
        const IMAGE_TOKEN: &[u8] = b"BI /W 1 /H 1 /BPC 1 /CS /G ID\n\x80\nEI";
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                [
                    b"BT /F1 12 Tf\n1 0 0 1 72 140 Tm\n(MIM) Tj\n1 0 0 1 72 126 Tm\n(MIMUS) Tj\nET\n"
                        .as_slice(),
                    RULE,
                    b"\n".as_slice(),
                    INLINE_IMAGE,
                    b"\nBT /F1 12 Tf\n1 0 0 1 220 140 Tm\n(U) Tj\n1 0 0 1 238 140 Tm\n(S) Tj\nET\n",
                ]
                .concat(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "\u{4e2d}{v1}\u{6587}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        chars.sort_by(|left, right| {
            right
                .baseline_origin
                .y
                .total_cmp(&left.baseline_origin.y)
                .then_with(|| left.baseline_origin.x.total_cmp(&right.baseline_origin.x))
        });
        let owner = Rect {
            left: 72.0,
            bottom: 110.0,
            right: 260.0,
            top: 152.0,
        };
        for character in &mut chars {
            character.layout.as_mut().unwrap().bounds = owner;
        }
        document.extracted_pages[0].layout_regions[0].bounds = owner;
        let formula = chars
            .iter_mut()
            .find(|character| character.unicode == Some('U'))
            .unwrap();
        let source_formula_x = formula.baseline_origin.x;
        let layout = formula.layout.as_mut().unwrap();
        layout.label = LayoutLabel::InlineFormula;
        layout.policy = TranslationPolicy::Passthrough;
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: owner,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: None,
            translation_conservation: None,
            preserved: None,
        }];

        styles_and_formulas(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        let rewrite = &document.rewrites[0];
        let relocated_x = rewrite
            .typeset_characters
            .iter()
            .find(|character| character.unicode == 'U')
            .unwrap()
            .baseline_origin
            .x;
        assert!(relocated_x + 50.0 < source_formula_x);
        let source_stream = &document.extracted_pages[0].content_streams[0].decoded;
        let rule_start = source_stream
            .windows(RULE.len())
            .position(|window| window == RULE)
            .unwrap();
        let rule_end = rule_start + RULE.len();
        let replacement = rewrite
            .replacements
            .iter()
            .find(|replacement| {
                replacement.byte_start == rule_start && replacement.byte_end == rule_end
            })
            .expect("the uniquely owned vector rule must be relocated with the formula glyph");
        assert!(
            replacement
                .replacement
                .windows(RULE.len())
                .any(|window| window == RULE),
            "the relocated rule must replay its exact source program"
        );
        let image_start = source_stream
            .windows(IMAGE_TOKEN.len())
            .position(|window| window == IMAGE_TOKEN)
            .unwrap();
        let image_replacement = rewrite
            .replacements
            .iter()
            .find(|replacement| {
                replacement.byte_start == image_start
                    && replacement.byte_end == image_start + IMAGE_TOKEN.len()
            })
            .expect("the uniquely owned inline image must move with the formula glyph");
        assert!(
            image_replacement
                .replacement
                .windows(IMAGE_TOKEN.len())
                .any(|window| window == IMAGE_TOKEN)
        );
    }

    #[test]
    fn formula_relocation_rejects_ink_owned_by_another_paragraph() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("cross-paragraph-formula-ink.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        const RULE: &[u8] = b"q\n1 0 0 1 218 148 cm\n0 0 m 16 0 l S\nQ";
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                [
                    b"BT /F1 12 Tf\n1 0 0 1 72 140 Tm\n(MIM) Tj\n1 0 0 1 72 126 Tm\n(MIMUS) Tj\nET\n"
                        .as_slice(),
                    RULE,
                    b"\nBT /F1 12 Tf\n1 0 0 1 220 140 Tm\n(U) Tj\n1 0 0 1 220 135 Tm\n(IM) Tj\n1 0 0 1 238 140 Tm\n(S) Tj\nET\n",
                ]
                .concat(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "\u{4e2d}{v1}\u{6587}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        chars.sort_by(|left, right| {
            right
                .baseline_origin
                .y
                .total_cmp(&left.baseline_origin.y)
                .then_with(|| left.baseline_origin.x.total_cmp(&right.baseline_origin.x))
        });
        let owner = Rect {
            left: 72.0,
            bottom: 110.0,
            right: 260.0,
            top: 152.0,
        };
        for character in &mut chars {
            character.layout.as_mut().unwrap().bounds = owner;
        }
        document.extracted_pages[0].layout_regions[0].bounds = owner;
        let (mut denominator, mut chars): (Vec<_>, Vec<_>) = chars
            .into_iter()
            .partition(|character| (character.baseline_origin.y - 135.0).abs() <= 0.01);
        assert_eq!(denominator.len(), 2);
        let numerator = chars
            .iter_mut()
            .find(|character| {
                character.unicode == Some('U')
                    && (character.baseline_origin.x - 220.0).abs() <= 0.01
            })
            .unwrap();
        for character in std::iter::once(numerator).chain(denominator.iter_mut()) {
            let layout = character.layout.as_mut().unwrap();
            layout.label = LayoutLabel::InlineFormula;
            layout.policy = TranslationPolicy::Passthrough;
        }
        let denominator_bounds = denominator
            .iter()
            .map(|character| character.r#box.union(character.visual_bbox))
            .reduce(Rect::union)
            .unwrap();
        document.il.pages[0].paragraphs = vec![
            Paragraph {
                reading_order: 0,
                bounds: owner,
                first_line_indent: None,
                text: TextCarrier::Chars { chars },
                translated_text: None,
                translation_conservation: None,
                preserved: None,
            },
            Paragraph {
                reading_order: 1,
                bounds: denominator_bounds,
                first_line_indent: None,
                text: TextCarrier::Chars { chars: denominator },
                translated_text: None,
                translation_conservation: None,
                preserved: Some(il::PreservedReason::TypesetProtocol),
            },
        ];

        styles_and_formulas(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::TypesetProtocol),
            "formula ink with a second paragraph owner must remain at its source geometry"
        );
    }

    #[test]
    fn formula_vector_scope_with_other_visible_content_fails_closed() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let character = &document.il.pages[0].paragraphs[0].chars()[0];
        let mut units = vec![SourceFormulaUnit {
            chars: vec![character],
            validation_characters: Vec::new(),
            spans: vec![(
                (character.passthrough.content_object, 0),
                character.passthrough.byte_start,
                character.passthrough.byte_end,
            )],
            split_glyphs: BTreeMap::new(),
            vector_paths: Vec::new(),
            inline_images: Vec::new(),
            bounds: character.r#box.union(character.visual_bbox),
            glyph_ink_bounds: vec![character.visual_bbox],
            ink_bounds: character.visual_bbox,
            source_fonts: vec![character.font.clone()],
        }];
        document.extracted_pages[0].vector_paths = vec![crate::walk::WalkedVectorPath {
            content_object: (character.passthrough.content_object, 0),
            byte_start: character.passthrough.byte_end + 1,
            byte_end: character.passthrough.byte_end + 20,
            start: il::Point {
                x: character.r#box.left,
                y: character.baseline_origin.y + 2.0,
            },
            end: il::Point {
                x: character.r#box.right,
                y: character.baseline_origin.y + 2.0,
            },
            content_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            safe_to_replay: false,
            form_clip: None,
            clips: Default::default(),
        }];

        assert!(
            attach_uniquely_owned_formula_ink(&mut units, &document.extracted_pages[0]).is_none(),
            "a formula-related path that cannot be replayed independently must fail closed"
        );

        let retained_xobject = crate::walk::WalkedInlineImage {
            content_object: (character.passthrough.content_object, 0),
            byte_start: character.passthrough.byte_end + 21,
            byte_end: character.passthrough.byte_end + 28,
            bounds: character.visual_bbox,
            content_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            replayable: false,
            form_clip: None,
            clips: Default::default(),
        };
        assert!(
            !formula_owns_inline_image(&units[0], &retained_xobject),
            "an Image XObject invocation is not a self-contained formula replay program"
        );
    }

    #[test]
    fn fraction_rule_between_numerator_and_denominator_moves_with_the_formula() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory
            .path()
            .join("fraction-vector-formula-relocation.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        const FRACTION_RULE: &[u8] = b"q\n1 0 0 1 218 145 cm\n0 0 m 24 0 l S\nQ";
        const RADICAL_RULE: &[u8] = b"q\n1 0 0 1 226 141 cm\n0 0 m 12 0 l S\nQ";
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                [
                    b"BT /F1 12 Tf\n1 0 0 1 72 140 Tm\n(MIM) Tj\n1 0 0 1 226 150 Tm\n(U) Tj\n1 0 0 1 72 126 Tm\n(MIMUS) Tj\nET\n"
                        .as_slice(),
                    FRACTION_RULE,
                    b"\nBT /F1 12 Tf\n1 0 0 1 220 135 Tm\n(U) Tj\nET\n".as_slice(),
                    RADICAL_RULE,
                    b"\nBT /F1 12 Tf\n1 0 0 1 228 135 Tm\n(M) Tj\n/F1 8 Tf\n1 0 0 1 237 132 Tm\n(I) Tj\n/F1 12 Tf\n1 0 0 1 250 135 Tm\n(S) Tj\nET\n",
                ]
                .concat(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "\u{4e2d}{v1}\u{6587}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        let owner = Rect {
            left: 72.0,
            bottom: 110.0,
            right: 270.0,
            top: 160.0,
        };
        for character in &mut chars {
            character.layout.as_mut().unwrap().bounds = owner;
            if (character.baseline_origin.x - 226.0).abs() <= 0.01
                && (character.baseline_origin.y - 150.0).abs() <= 0.01
            {
                character.unicode = Some('1');
                character.layout.as_mut().unwrap().source = LayoutSource::Model;
                continue;
            }
            if character.baseline_origin.x >= 220.0 && character.baseline_origin.x < 245.0 {
                let layout = character.layout.as_mut().unwrap();
                layout.label = LayoutLabel::InlineFormula;
                layout.policy = TranslationPolicy::Passthrough;
                layout.reading_order = 7;
                layout.source = LayoutSource::Model;
            }
        }
        document.extracted_pages[0].layout_regions[0].bounds = owner;
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: owner,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: None,
            translation_conservation: None,
            preserved: None,
        }];

        styles_and_formulas(&mut document, &context).unwrap();
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        chars
            .iter_mut()
            .find(|character| character.unicode == Some('1'))
            .unwrap()
            .unicode = Some('U');
        let content_objects = document.extracted_pages[0]
            .content_streams
            .iter()
            .map(|stream| stream.object_id)
            .collect::<BTreeSet<_>>();
        let content_object_numbers = content_objects
            .iter()
            .map(|object_id| object_id.0)
            .collect::<BTreeSet<_>>();
        let units = source_formula_units(
            &document.il.pages[0].paragraphs[0],
            &document.extracted_pages[0],
            &content_objects,
            &content_object_numbers,
            true,
        )
        .expect("the complete composite formula must have a relocatable unit");
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].vector_paths.len(), 2);
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        let rewrite = &document.rewrites[0];
        let source_stream = &document.extracted_pages[0].content_streams[0].decoded;
        for rule in [FRACTION_RULE, RADICAL_RULE] {
            let start = source_stream
                .windows(rule.len())
                .position(|window| window == rule)
                .unwrap();
            let replacement = rewrite
                .replacements
                .iter()
                .find(|replacement| {
                    replacement.byte_start == start && replacement.byte_end == start + rule.len()
                })
                .expect("every uniquely owned formula rule must leave its source slot");
            assert!(
                replacement
                    .replacement
                    .windows(rule.len())
                    .any(|w| w == rule)
            );
        }
    }

    #[test]
    fn formula_vector_ownership_rejects_a_wider_neighboring_table_rule() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let character = &document.il.pages[0].paragraphs[0].chars()[0];
        let unit = SourceFormulaUnit {
            chars: vec![character],
            validation_characters: Vec::new(),
            spans: vec![(
                (character.passthrough.content_object, 0),
                character.passthrough.byte_start,
                character.passthrough.byte_end,
            )],
            split_glyphs: BTreeMap::new(),
            vector_paths: Vec::new(),
            inline_images: Vec::new(),
            bounds: character.r#box.union(character.visual_bbox),
            glyph_ink_bounds: vec![character.visual_bbox],
            ink_bounds: character.visual_bbox,
            source_fonts: vec![character.font.clone()],
        };
        let path = crate::walk::WalkedVectorPath {
            content_object: (character.passthrough.content_object, 0),
            byte_start: character.passthrough.byte_start.saturating_sub(16),
            byte_end: character.passthrough.byte_start.saturating_sub(1),
            start: il::Point {
                x: character.r#box.left - 12.0,
                y: character.baseline_origin.y + 8.0,
            },
            end: il::Point {
                x: character.r#box.right + 24.0,
                y: character.baseline_origin.y + 8.0,
            },
            content_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            safe_to_replay: true,
            form_clip: None,
            clips: Default::default(),
        };

        assert!(
            !formula_owns_vector_path(&unit, &path),
            "a table or decoration rule wider than the formula must remain independent"
        );
    }

    #[test]
    fn fraction_rule_expands_its_unique_numerator_into_the_model_formula() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("fraction-numerator-boundary.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 140 Tm\n(MIM) Tj\n1 0 0 1 226 150 Tm\n(1) Tj\n1 0 0 1 72 126 Tm\n(MIMUS) Tj\nET\nq\n1 0 0 1 220 145 cm\n0 0 m 25 0 l S\nQ\nBT /F1 12 Tf\n1 0 0 1 220 135 Tm\n(U) Tj\n1 0 0 1 230 135 Tm\n(M) Tj\n/F1 8 Tf\n1 0 0 1 239 132 Tm\n(I) Tj\n/F1 12 Tf\n1 0 0 1 250 135 Tm\n(S) Tj\nET\n"
                    .to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();

        let mut chars = document.il.pages[0]
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().iter().cloned())
            .collect::<Vec<_>>();
        let owner = Rect {
            left: 72.0,
            bottom: 110.0,
            right: 270.0,
            top: 160.0,
        };
        for character in &mut chars {
            character.layout.as_mut().unwrap().bounds = owner;
            if (character.baseline_origin.x - 226.0).abs() <= 0.01
                && (character.baseline_origin.y - 150.0).abs() <= 0.01
            {
                // MimusExact does not carry a digit glyph; the boundary contract is
                // independent of font bytes, so give this hand-built numerator its
                // intended decoded scalar after the production walk.
                character.unicode = Some('1');
                character.layout.as_mut().unwrap().source = LayoutSource::Model;
            }
            if matches!(character.unicode, Some('U' | 'M' | 'I'))
                && character.baseline_origin.x >= 220.0
            {
                let layout = character.layout.as_mut().unwrap();
                layout.label = LayoutLabel::InlineFormula;
                layout.policy = TranslationPolicy::Passthrough;
                layout.reading_order = 7;
                layout.source = LayoutSource::Model;
            }
        }
        document.extracted_pages[0].layout_regions[0].bounds = owner;
        document.il.pages[0].paragraphs = vec![Paragraph {
            reading_order: 0,
            bounds: owner,
            first_line_indent: None,
            text: TextCarrier::Chars { chars },
            translated_text: None,
            translation_conservation: None,
            preserved: None,
        }];

        styles_and_formulas(&mut document, &context).unwrap();

        let paragraph = &document.il.pages[0].paragraphs[0];
        let numerator = paragraph
            .chars()
            .iter()
            .find(|character| character.unicode == Some('1'))
            .unwrap();
        assert_eq!(
            numerator.layout.unwrap().label,
            LayoutLabel::InlineFormula,
            "the proven numerator must be part of the model-owned formula boundary"
        );
        let request = document
            .prepared_translations
            .get(&(0, 0))
            .unwrap()
            .request_text();
        assert_eq!(request.matches("{v").count(), 1);
        assert!(
            !request.replace("{v1}", "").contains('1'),
            "formula numerator leaked outside its placeholder in {request:?}"
        );
    }

    #[test]
    fn extraction_order_punctuation_is_moved_after_the_following_formula_unit() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        {
            let TextCarrier::Chars { chars } = &mut paragraph.text;
            chars.truncate(3);
            chars[1].unicode = Some('.');
            chars[1].r#box = Rect {
                left: 118.0,
                bottom: 98.0,
                right: 124.0,
                top: 110.0,
            };
            chars[1].visual_bbox = chars[1].r#box;
        }
        let TextCarrier::Chars { chars } = &paragraph.text;
        let mut source_segments = vec![Vec::new(), vec![&chars[1]], Vec::new()];
        let formulas = [
            FormulaContinuityFormula {
                formula_index: 0,
                bounds: Rect {
                    left: 100.0,
                    bottom: 98.0,
                    right: 108.0,
                    top: 110.0,
                },
                line_left: 100.0,
            },
            FormulaContinuityFormula {
                formula_index: 1,
                bounds: Rect {
                    left: 109.0,
                    bottom: 98.0,
                    right: 117.0,
                    top: 110.0,
                },
                line_left: 100.0,
            },
        ];
        let styled = |value| crate::translate::StyledCharacter { value, bold: false };
        let mut translated_segments = vec![
            Vec::new(),
            "\u{7f29}\u{653e}\u{70b9}\u{79ef}\u{3002}"
                .chars()
                .map(styled)
                .collect(),
            Vec::new(),
        ];

        normalize_formula_interleaved_punctuation_order(
            paragraph,
            &BTreeSet::new(),
            &mut source_segments,
            &formulas,
            &mut translated_segments,
            18.0,
        );

        assert!(source_segments[1].is_empty());
        assert_eq!(source_segments[2][0].unicode, Some('.'));
        assert!(translated_segments[1].is_empty());
        assert_eq!(
            translated_segments[2]
                .iter()
                .map(|character| character.value)
                .collect::<String>(),
            "\u{7f29}\u{653e}\u{70b9}\u{79ef}\u{3002}"
        );
    }

    #[test]
    fn model_owned_formula_tail_moves_the_intervening_line_after_the_operand() {
        // (3,9) differs from the adjacent-unit fixture at (4,21): the extractor
        // emits `sqrt`, then the rest of the visual line, and only then `d_k`.
        // The model assigns sqrt and d to one formula region, while geometry
        // proves that the intervening line starts after the d_k operand.
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let formula_layout = LayoutAssignment {
            label: LayoutLabel::InlineFormula,
            reading_order: 99,
            bounds: Rect {
                left: 100.0,
                bottom: 98.0,
                right: 117.0,
                top: 110.0,
            },
            source: LayoutSource::Model,
            policy: TranslationPolicy::Passthrough,
        };
        {
            let TextCarrier::Chars { chars } = &mut paragraph.text;
            chars.push(chars.last().expect("fixture has characters").clone());
            chars.truncate(6);
            for (index, (unicode, bounds)) in [
                (
                    '√',
                    Rect {
                        left: 100.0,
                        bottom: 98.0,
                        right: 108.0,
                        top: 110.0,
                    },
                ),
                (
                    '.',
                    Rect {
                        left: 118.0,
                        bottom: 98.0,
                        right: 120.0,
                        top: 110.0,
                    },
                ),
                (
                    'A',
                    Rect {
                        left: 120.0,
                        bottom: 98.0,
                        right: 150.0,
                        top: 110.0,
                    },
                ),
                (
                    'd',
                    Rect {
                        left: 109.0,
                        bottom: 98.0,
                        right: 113.0,
                        top: 110.0,
                    },
                ),
                (
                    'k',
                    Rect {
                        left: 113.0,
                        bottom: 96.0,
                        right: 117.0,
                        top: 104.0,
                    },
                ),
                (
                    't',
                    Rect {
                        left: 100.0,
                        bottom: 84.0,
                        right: 105.0,
                        top: 94.0,
                    },
                ),
            ]
            .into_iter()
            .enumerate()
            {
                chars[index].unicode = Some(unicode);
                chars[index].r#box = bounds;
                chars[index].visual_bbox = bounds;
                chars[index].layout = Some(if matches!(index, 0 | 3 | 4) {
                    formula_layout
                } else {
                    LayoutAssignment {
                        label: LayoutLabel::Text,
                        reading_order: 1,
                        bounds: paragraph.bounds,
                        source: LayoutSource::Model,
                        policy: TranslationPolicy::Translate,
                    }
                });
            }
        }
        let content_objects = paragraph
            .chars()
            .iter()
            .map(|character| character.passthrough.content_object)
            .collect::<BTreeSet<_>>();
        let TextCarrier::Chars { chars } = &paragraph.text;
        let mut source_segments = vec![Vec::new(), vec![&chars[1], &chars[2]], vec![&chars[5]]];
        let formulas = [
            FormulaContinuityFormula {
                formula_index: 0,
                bounds: chars[0].r#box,
                line_left: 100.0,
            },
            FormulaContinuityFormula {
                formula_index: 1,
                bounds: chars[3].r#box.union(chars[4].r#box),
                line_left: 100.0,
            },
        ];
        let styled = |value| crate::translate::StyledCharacter { value, bold: false };
        let mut translated_segments = vec![
            Vec::new(),
            ".after".chars().map(styled).collect(),
            "tail".chars().map(styled).collect(),
        ];

        normalize_formula_interleaved_punctuation_order(
            paragraph,
            &content_objects,
            &mut source_segments,
            &formulas,
            &mut translated_segments,
            18.0,
        );

        assert!(source_segments[1].is_empty());
        assert_eq!(
            source_segments[2]
                .iter()
                .filter_map(|character| character.unicode)
                .collect::<String>(),
            ".At"
        );
        assert_eq!(
            translated_segments[2]
                .iter()
                .map(|character| character.value)
                .collect::<String>(),
            ".aftertail"
        );
    }

    #[test]
    fn fragmented_model_formula_is_coalesced_at_its_unique_visual_gap() {
        // A fraction/radical can be emitted as numerator, prose, radical, prose,
        // then operand. Unlike adjacent formulas, all four formula glyphs belong
        // to one model region and geometry places that region in one unique gap.
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let template = paragraph.chars()[0].clone();
        let text_layout = LayoutAssignment {
            label: LayoutLabel::Text,
            reading_order: 1,
            bounds: paragraph.bounds,
            source: LayoutSource::Model,
            policy: TranslationPolicy::Translate,
        };
        let formula_layout = LayoutAssignment {
            label: LayoutLabel::InlineFormula,
            reading_order: 99,
            bounds: Rect {
                left: 118.0,
                bottom: 94.0,
                right: 138.0,
                top: 112.0,
            },
            source: LayoutSource::Model,
            policy: TranslationPolicy::Passthrough,
        };
        let chars = [
            ('A', 100.0, 100.0, false),
            ('1', 125.0, 108.0, true),
            ('o', 108.0, 100.0, false),
            ('f', 113.0, 100.0, false),
            ('\u{221a}', 120.0, 101.0, true),
            ('.', 139.0, 100.0, false),
            ('B', 144.0, 100.0, false),
            ('d', 127.0, 96.0, true),
            ('k', 132.0, 94.0, true),
            ('C', 100.0, 84.0, false),
        ]
        .into_iter()
        .map(|(unicode, x, y, formula)| {
            let mut character = template.clone();
            character.unicode = Some(unicode);
            character.baseline_origin = il::Point { x, y };
            character.r#box = Rect {
                left: x,
                bottom: y - 2.0,
                right: x + 4.0,
                top: y + 8.0,
            };
            character.visual_bbox = character.r#box;
            character.layout = Some(if formula { formula_layout } else { text_layout });
            character
        })
        .collect::<Vec<_>>();
        paragraph.text = TextCarrier::Chars { chars };
        let content_objects = paragraph
            .chars()
            .iter()
            .map(|character| character.passthrough.content_object)
            .collect::<BTreeSet<_>>();

        complete_model_formula_boundaries(paragraph, &content_objects, 0, 0, &mut Vec::new());

        let formula = paragraph
            .chars()
            .iter()
            .filter(|character| {
                prepared_character_class(character, &content_objects)
                    == PreparedCharacterClass::Formula
            })
            .filter_map(|character| character.unicode)
            .collect::<String>();
        assert_eq!(formula, "1\u{221a}dk");
        assert_eq!(
            source_text_segments(paragraph.chars(), &content_objects).len(),
            2,
            "one model-owned rigid formula must produce one placeholder"
        );
        assert_eq!(paragraph.source_text(), "Aof1\u{221a}dk.BC");
    }

    #[test]
    fn formula_continuity_limit_is_derived_from_source_spacing_and_em() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        {
            let TextCarrier::Chars { chars } = &mut paragraph.text;
            for character in chars.iter_mut() {
                character.font_size = 12.0;
                character.implicit_space_before = false;
            }
        }
        let content_objects = paragraph
            .chars()
            .iter()
            .map(|character| character.passthrough.content_object)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            formula_continuity_limit(paragraph, &content_objects),
            Some(18.0)
        );

        let TextCarrier::Chars { chars } = &mut paragraph.text;
        let first_right = chars[0].r#box.right;
        let width = chars[1].r#box.right - chars[1].r#box.left;
        chars[1].r#box.left = first_right + 12.0;
        chars[1].r#box.right = chars[1].r#box.left + width;
        chars[1].implicit_space_before = true;
        assert_eq!(
            formula_continuity_limit(paragraph, &content_objects),
            Some(24.0)
        );
    }

    #[test]
    fn shared_formula_continuity_oracle_rejects_split_punctuation_and_reordered_units() {
        let styled = |value| crate::translate::StyledCharacter { value, bold: false };
        let translated = vec![vec![styled('\u{4e2d}')], vec![styled('\u{ff0c}')]];
        let text = vec![
            FormulaContinuityText {
                segment_index: 0,
                lines: vec![FormulaContinuityLine {
                    bounds: Rect {
                        left: 72.0,
                        bottom: 98.0,
                        right: 84.0,
                        top: 110.0,
                    },
                    line_left: 72.0,
                    starts_with_punctuation: false,
                    ends_with_punctuation: false,
                }],
            },
            FormulaContinuityText {
                segment_index: 1,
                lines: vec![FormulaContinuityLine {
                    bounds: Rect {
                        left: 72.0,
                        bottom: 82.0,
                        right: 84.0,
                        top: 94.0,
                    },
                    line_left: 72.0,
                    starts_with_punctuation: true,
                    ends_with_punctuation: true,
                }],
            },
        ];
        let formula = FormulaContinuityFormula {
            formula_index: 0,
            bounds: Rect {
                left: 84.0,
                bottom: 98.0,
                right: 94.0,
                top: 110.0,
            },
            line_left: 72.0,
        };
        assert!(!formula_continuity_is_valid(
            &translated,
            &text,
            &[formula],
            18.0,
        ));

        let translated = vec![Vec::new(), Vec::new(), Vec::new()];
        let formulas = [
            FormulaContinuityFormula {
                formula_index: 0,
                bounds: Rect {
                    left: 90.0,
                    bottom: 82.0,
                    right: 100.0,
                    top: 94.0,
                },
                line_left: 72.0,
            },
            FormulaContinuityFormula {
                formula_index: 1,
                bounds: Rect {
                    left: 90.0,
                    bottom: 98.0,
                    right: 100.0,
                    top: 110.0,
                },
                line_left: 72.0,
            },
        ];
        assert!(!formula_continuity_is_valid(
            &translated,
            &[],
            &formulas,
            18.0,
        ));
    }

    #[test]
    fn formula_continuity_oracle_rejects_extraction_order_text_after_following_formula() {
        // `(4,21)` attached a radical from a text segment to one model formula.
        // L5-5R `(3,9)` instead has two model formula units with an intervening
        // extracted text segment whose geometry follows both units on the same line.
        let styled = |value| crate::translate::StyledCharacter { value, bold: false };
        let translated = vec![Vec::new(), vec![styled('\u{3002}')], Vec::new()];
        let text = [FormulaContinuityText {
            segment_index: 1,
            lines: vec![FormulaContinuityLine {
                bounds: Rect {
                    left: 118.0,
                    bottom: 98.0,
                    right: 124.0,
                    top: 110.0,
                },
                line_left: 100.0,
                starts_with_punctuation: true,
                ends_with_punctuation: true,
            }],
        }];
        let formulas = [
            FormulaContinuityFormula {
                formula_index: 0,
                bounds: Rect {
                    left: 100.0,
                    bottom: 98.0,
                    right: 108.0,
                    top: 110.0,
                },
                line_left: 100.0,
            },
            FormulaContinuityFormula {
                formula_index: 1,
                bounds: Rect {
                    left: 109.0,
                    bottom: 98.0,
                    right: 117.0,
                    top: 110.0,
                },
                line_left: 100.0,
            },
        ];

        assert!(!formula_continuity_is_valid(
            &translated,
            &text,
            &formulas,
            18.0,
        ));
    }

    #[test]
    fn formula_punctuation_group_does_not_split_across_slots() {
        let output_fonts = test_output_fonts();
        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let formula_bounds = Rect {
            left: 0.0,
            bottom: 0.0,
            right: 10.0,
            top: 12.0,
        };
        let formula_units = [SourceFormulaUnit {
            chars: Vec::new(),
            validation_characters: Vec::new(),
            spans: Vec::new(),
            split_glyphs: BTreeMap::new(),
            vector_paths: Vec::new(),
            inline_images: Vec::new(),
            bounds: formula_bounds,
            glyph_ink_bounds: vec![formula_bounds],
            ink_bounds: formula_bounds,
            source_fonts: Vec::new(),
        }];
        let atoms = [
            FormulaFlowAtom::Formula(0),
            FormulaFlowAtom::Text {
                segment_index: 1,
                characters: vec![crate::translate::StyledCharacter {
                    value: '\u{ff0c}',
                    bold: false,
                }],
                extra_advance_pt: 0.0,
                section_prefix_only: false,
            },
        ];
        let slots = [TypesetLineSlot {
            left: 0.0,
            right: 15.0,
            baseline_y: 10.0,
        }];

        assert!(matches!(
            place_formula_flow(&atoms, &formula_units, &faces, 12.0, &slots, None, false),
            Some(FormulaFlowAttempt::NoFit)
        ));
    }

    #[test]
    fn formula_flow_geometry_retries_smaller_font_before_continuity_oracle() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &context).unwrap();

        let output_fonts = test_output_fonts();
        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let preferred = 12.0;
        let smaller = preferred - 0.5;
        let styled = |value| crate::translate::StyledCharacter { value, bold: false };
        let translated_segments = vec![
            Vec::new(),
            vec![
                styled(','),
                styled(' '),
                styled('中'),
                styled(' '),
                styled('文'),
            ],
        ];

        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        for character in chars.iter_mut() {
            character.font_size = preferred;
        }
        let formula = &mut chars[0];
        let formula_bounds = formula.r#box.union(formula.visual_bbox);
        let layout = formula.layout.as_mut().unwrap();
        layout.label = LayoutLabel::InlineFormula;
        layout.policy = TranslationPolicy::Passthrough;

        let comma = [styled(',')];
        let preferred_chain_width = formula_bounds.right - formula_bounds.left
            + styled_token_width(&comma, &faces, preferred).unwrap();
        let smaller_chain_width = formula_bounds.right - formula_bounds.left
            + styled_token_width(&comma, &faces, smaller).unwrap();
        let first_slot_width = (preferred_chain_width + smaller_chain_width) / 2.0;
        let remainder_width = styled_token_width(
            &[styled(' '), styled('中'), styled(' '), styled('文')],
            &faces,
            preferred,
        )
        .unwrap();
        let baseline_y = chars[1].baseline_origin.y;
        let container = Rect {
            left: 72.0,
            bottom: baseline_y + faces.descent_em() * preferred,
            right: 72.0 + first_slot_width + 5.0 + remainder_width + 1.0,
            top: baseline_y + faces.ascent_em() * preferred,
        };
        for character in chars.iter_mut() {
            character.layout.as_mut().unwrap().bounds = container;
        }
        paragraph.bounds = container;

        let content_objects = paragraph
            .chars()
            .iter()
            .map(|character| (character.passthrough.content_object, 0))
            .collect::<BTreeSet<_>>();
        let content_object_numbers = content_objects
            .iter()
            .map(|object_id| object_id.0)
            .collect::<BTreeSet<_>>();
        let source_segments = source_text_segments(paragraph.chars(), &content_object_numbers);
        assert_eq!(source_segments.len(), 2);
        let obstacle = Rect {
            left: container.left + first_slot_width,
            bottom: container.bottom - 1.0,
            right: container.left + first_slot_width + 5.0,
            top: container.top + 1.0,
        };
        let page_bounds = Rect {
            left: 0.0,
            bottom: 0.0,
            right: 612.0,
            top: 792.0,
        };

        let plan = plan_relocated_formula_flow(
            paragraph,
            &source_segments,
            &translated_segments,
            None,
            &document.extracted_pages[0],
            &content_objects,
            &output_fonts,
            page_bounds,
            &[obstacle],
        )
        .expect("the next smaller geometric candidate keeps formula punctuation together");

        assert!((plan.font_size - smaller).abs() <= 0.001);
        assert!(ink_bounds_are_safe(
            &plan.ink_bounds,
            page_bounds,
            &[obstacle]
        ));
    }

    #[test]
    fn adjacent_radical_and_operand_formula_units_are_placed_atomically() {
        let output_fonts = test_output_fonts();
        let faces = OutputFontFaces::parse(&output_fonts).unwrap();
        let formula_unit = |unicode, left| {
            let bounds = Rect {
                left,
                bottom: 0.0,
                right: left + 8.0,
                top: 12.0,
            };
            SourceFormulaUnit {
                chars: Vec::new(),
                validation_characters: vec![TypesetCharacter {
                    unicode,
                    baseline_origin: il::Point { x: left, y: 0.0 },
                }],
                spans: Vec::new(),
                split_glyphs: BTreeMap::new(),
                vector_paths: Vec::new(),
                inline_images: Vec::new(),
                bounds,
                glyph_ink_bounds: vec![bounds],
                ink_bounds: bounds,
                source_fonts: Vec::new(),
            }
        };
        let formula_units = [formula_unit('\u{221a}', 0.0), formula_unit('d', 8.0)];
        let atoms = [FormulaFlowAtom::Formula(0), FormulaFlowAtom::Formula(1)];
        let slots = [
            TypesetLineSlot {
                left: 0.0,
                right: 10.0,
                baseline_y: 24.0,
            },
            TypesetLineSlot {
                left: 0.0,
                right: 20.0,
                baseline_y: 10.0,
            },
        ];

        let FormulaFlowAttempt::Placed(placement) =
            place_formula_flow(&atoms, &formula_units, &faces, 12.0, &slots, None, false).unwrap()
        else {
            panic!("the adjacent formula group fits in the second slot");
        };
        let relocated = placement
            .formula_relocations
            .iter()
            .map(|relocation| relocation.characters[0].baseline_origin)
            .collect::<Vec<_>>();
        assert_eq!(relocated.len(), 2);
        assert!((relocated[0].y - relocated[1].y).abs() <= 0.01);
        assert!((relocated[1].x - relocated[0].x - 8.0).abs() <= 0.01);
        assert!(matches!(
            place_formula_flow(&atoms, &formula_units, &faces, 12.0, &slots, None, true),
            Some(FormulaFlowAttempt::NoFit)
        ));
    }

    #[test]
    fn uniquely_matched_translated_radical_moves_beside_its_source_formula_operand() {
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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars.truncate(5);
        chars[1].unicode = Some('\u{221a}');
        chars[1].r#box = Rect {
            left: 92.0,
            bottom: 100.0,
            right: 100.0,
            top: 112.0,
        };
        chars[1].visual_bbox = chars[1].r#box;
        let mut source_segments = vec![chars[..4].iter().collect::<Vec<_>>(), Vec::new()];
        let formulas = [FormulaContinuityFormula {
            formula_index: 0,
            bounds: Rect {
                left: 100.0,
                bottom: 98.0,
                right: 108.0,
                top: 110.0,
            },
            line_left: 72.0,
        }];
        let styled = |value| crate::translate::StyledCharacter { value, bold: false };
        let mut translated_segments = vec![
            vec![styled('\u{4e2d}'), styled('\u{221a}'), styled('\u{6587}')],
            Vec::new(),
        ];

        attach_translated_radicals_to_formula_operands(
            &mut source_segments,
            &formulas,
            &mut translated_segments,
        )
        .unwrap();

        assert_eq!(
            translated_segments[0]
                .iter()
                .map(|character| character.value)
                .collect::<String>(),
            "\u{4e2d}\u{6587}"
        );
        assert!(
            source_segments[0]
                .iter()
                .all(|character| character.unicode != Some('\u{221a}'))
        );
    }

    #[test]
    fn source_order_displaced_radical_attaches_to_its_unique_visual_formula() {
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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars.truncate(5);
        chars[1].unicode = Some('\u{221a}');
        chars[1].r#box = Rect {
            left: 92.0,
            bottom: 100.0,
            right: 100.0,
            top: 112.0,
        };
        chars[1].visual_bbox = chars[1].r#box;

        // pdfTeX can emit a radical before an unrelated formula in extraction order even though
        // its visual ink belongs to a later operand. This is the shape from 1706 page 4 (3,4).
        let mut source_segments = vec![Vec::new(), vec![&chars[1]], Vec::new(), Vec::new()];
        let formulas = [
            FormulaContinuityFormula {
                formula_index: 0,
                bounds: Rect {
                    left: 20.0,
                    bottom: 98.0,
                    right: 28.0,
                    top: 110.0,
                },
                line_left: 72.0,
            },
            FormulaContinuityFormula {
                formula_index: 1,
                bounds: Rect {
                    left: 200.0,
                    bottom: 98.0,
                    right: 208.0,
                    top: 110.0,
                },
                line_left: 72.0,
            },
            FormulaContinuityFormula {
                formula_index: 2,
                bounds: Rect {
                    left: 100.0,
                    bottom: 98.0,
                    right: 108.0,
                    top: 110.0,
                },
                line_left: 72.0,
            },
        ];
        let mut translated_segments = vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()];

        attach_translated_radicals_to_formula_operands(
            &mut source_segments,
            &formulas,
            &mut translated_segments,
        )
        .unwrap();

        assert!(
            source_segments
                .iter()
                .flatten()
                .all(|character| { character.unicode != Some('\u{221a}') })
        );
    }

    #[test]
    fn source_order_displaced_radical_expands_the_visual_formula_rigid_body() {
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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        let template = chars[0].clone();
        while chars.len() < 7 {
            chars.push(template.clone());
        }
        chars.truncate(7);
        for character in chars.iter_mut() {
            let layout = character.layout.as_mut().unwrap();
            layout.label = LayoutLabel::Text;
            layout.policy = TranslationPolicy::Translate;
        }
        for (index, left) in [(1, 20.0), (3, 200.0), (5, 100.0)] {
            chars[index].r#box = Rect {
                left,
                bottom: 100.0,
                right: left + 8.0,
                top: 110.0,
            };
            chars[index].visual_bbox = chars[index].r#box;
            let layout = chars[index].layout.as_mut().unwrap();
            layout.label = LayoutLabel::InlineFormula;
            layout.policy = TranslationPolicy::Passthrough;
        }
        chars[2].unicode = Some('\u{221a}');
        chars[2].r#box = Rect {
            left: 92.0,
            bottom: 100.0,
            right: 100.0,
            top: 112.0,
        };
        chars[2].visual_bbox = chars[2].r#box;
        let content_objects = BTreeSet::from([chars[0].passthrough.content_object]);

        let formulas = fixed_formula_continuity(paragraph, &content_objects).unwrap();

        assert_eq!(formulas.len(), 3);
        assert!((formulas[2].bounds.left - 92.0).abs() <= 0.01);
        assert!((formulas[2].bounds.right - 108.0).abs() <= 0.01);
    }

    #[test]
    fn ambiguous_translated_radical_attachment_fails_closed() {
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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars.truncate(5);
        chars[1].unicode = Some('\u{221a}');
        chars[1].r#box = Rect {
            left: 92.0,
            bottom: 100.0,
            right: 100.0,
            top: 112.0,
        };
        chars[1].visual_bbox = chars[1].r#box;
        let mut source_segments = vec![chars[..4].iter().collect::<Vec<_>>(), Vec::new()];
        let formulas = [FormulaContinuityFormula {
            formula_index: 0,
            bounds: Rect {
                left: 100.0,
                bottom: 98.0,
                right: 108.0,
                top: 110.0,
            },
            line_left: 72.0,
        }];
        let styled = |value| crate::translate::StyledCharacter { value, bold: false };
        let mut translated_segments = vec![
            vec![styled('\u{221a}'), styled('A'), styled('\u{221a}')],
            Vec::new(),
        ];

        assert!(
            attach_translated_radicals_to_formula_operands(
                &mut source_segments,
                &formulas,
                &mut translated_segments,
            )
            .is_none()
        );
        assert_eq!(
            translated_segments[0]
                .iter()
                .map(|character| character.value)
                .collect::<String>(),
            "\u{221a}A\u{221a}"
        );
        assert!(
            source_segments[0]
                .iter()
                .any(|character| character.unicode == Some('\u{221a}'))
        );
    }

    #[test]
    fn formula_boundary_expands_through_a_contiguous_digit_run() {
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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars.truncate(5);
        for (index, (character, unicode)) in
            chars.iter_mut().zip(['h', '=', '6', '4', '.']).enumerate()
        {
            character.unicode = Some(unicode);
            character.implicit_space_before = false;
            let layout = character.layout.as_mut().unwrap();
            layout.source = LayoutSource::Model;
            if index <= 2 {
                layout.label = LayoutLabel::InlineFormula;
                layout.policy = TranslationPolicy::Passthrough;
            } else {
                layout.label = LayoutLabel::Text;
                layout.policy = TranslationPolicy::Translate;
            }
        }
        let content_objects = BTreeSet::from([chars[0].passthrough.content_object]);
        let mut diagnostics = Vec::new();

        complete_model_formula_boundaries(paragraph, &content_objects, 0, 0, &mut diagnostics);

        assert_eq!(
            paragraph.chars()[3].layout.unwrap().label,
            LayoutLabel::InlineFormula
        );
        assert_eq!(
            paragraph.chars()[4].layout.unwrap().label,
            LayoutLabel::Text,
            "the sentence punctuation is not part of the digit run"
        );
        let evidence = diagnostics
            .iter()
            .map(|diagnostic| serde_json::to_value(diagnostic).unwrap())
            .filter_map(|diagnostic| diagnostic["evidence"].as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            evidence,
            BTreeSet::from(["contiguous_digit_run".to_owned()])
        );
    }

    #[test]
    fn repeated_content_conservation_failure_preserves_the_whole_paragraph() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "translated without the number",
            calls: AtomicUsize::new(0),
        };
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
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        chars[0].unicode = Some('4');

        styles_and_formulas(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();

        assert_eq!(translator.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::ContentConservation)
        );
        assert_eq!(document.il.pages[0].paragraphs[0].translated_text, None);
        assert!(
            document
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| matches!(
                    diagnostic,
                    Diagnostic::ContentConservationRetry {
                        missing_token_count: 1,
                        missing_tokens,
                        ..
                    } if missing_tokens == &["4".to_owned()]
                ))
        );
        assert!(
            document
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| matches!(
                    diagnostic,
                    Diagnostic::ContentConservationViolation {
                        missing_token_count: 1,
                        missing_tokens,
                        ..
                    } if missing_tokens == &["4".to_owned()]
                ))
        );
    }

    #[test]
    fn mixed_formula_text_uses_formula_separated_slots() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("mixed-formula-wide-slots.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf\n1 0 0 1 72 120 Tm\n(M) Tj (I) Tj (M) Tj (U) Tj (S) Tj\nET\n"
                    .to_vec(),
            );
        pdf.save(&input).unwrap();
        let mut document = Document::for_inspection(&input);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "M{v1}M{v2}\u{7ed3}\u{6784}\u{7a33}\u{5b9a}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();

        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        assert_eq!(chars.len(), 5);
        for character in chars.iter_mut() {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 90.0,
                right: 260.0,
                top: 135.0,
            };
        }
        document.extracted_pages[0].layout_regions[0].bounds = Rect {
            left: 72.0,
            bottom: 90.0,
            right: 260.0,
            top: 135.0,
        };
        for index in [1, 3] {
            let layout = chars[index].layout.as_mut().unwrap();
            layout.label = LayoutLabel::InlineFormula;
            layout.policy = TranslationPolicy::Passthrough;
        }

        styles_and_formulas(&mut document, &context).unwrap();
        assert_eq!(
            document
                .prepared_translations
                .values()
                .next()
                .unwrap()
                .request_text(),
            "M{v1}M{v2}S"
        );
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        assert_eq!(
            document.rewrites[0]
                .typeset_characters
                .iter()
                .map(|character| character.unicode)
                .collect::<String>(),
            "MM\u{7ed3}\u{6784}\u{7a33}\u{5b9a}"
        );
        let retained_formula_ink = document.il.pages[0].paragraphs[0]
            .chars()
            .iter()
            .filter(|character| {
                character.layout.is_some_and(|layout| {
                    layout.label == LayoutLabel::InlineFormula
                        && layout.policy == TranslationPolicy::Passthrough
                })
            })
            .map(|character| character.visual_bbox)
            .collect::<Vec<_>>();
        assert_typeset_ink_is_disjoint(
            &document.rewrites[0].typeset_ink_bounds,
            &retained_formula_ink,
        );
    }

    #[test]
    fn restored_body_bold_ranges_use_distinct_embedded_fonts() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "\u{4e2d}<b1>\u{6587}</b1>\u{6d4b}",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: config_with_test_output_fonts(),
        };
        inspect(&mut document, &context).unwrap();
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        for character in chars.iter_mut() {
            character.layout.as_mut().unwrap().bounds = Rect {
                left: 72.0,
                bottom: 90.0,
                right: 172.0,
                top: 135.0,
            };
        }
        document.extracted_pages[0].layout_regions[0].bounds = Rect {
            left: 72.0,
            bottom: 90.0,
            right: 172.0,
            top: 135.0,
        };
        chars[1].font.resource_name = "BodyBold".to_owned();

        styles_and_formulas(&mut document, &context).unwrap();
        assert_eq!(
            document
                .prepared_translations
                .get(&(0, 0))
                .unwrap()
                .request_text(),
            "M<b1>I</b1>MUS"
        );
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert!(
            document.il.pages[0].paragraphs[0].preserved.is_none(),
            "{:?}",
            document.il.pages[0].paragraphs[0].preserved
        );
        let fonts = &document.rewrites[0].embedded_fonts;
        assert_eq!(fonts.len(), 2);
        assert!(
            fonts
                .iter()
                .any(|font| font.base_font.ends_with("-Regular"))
        );
        assert!(fonts.iter().any(|font| font.base_font.ends_with("-Bold")));
        assert_eq!(document.rewrites[0].typeset_characters.len(), 3);
    }

    #[test]
    fn every_placeholder_protocol_failure_preserves_the_whole_paragraph() {
        for (output, expected, formula_count) in [
            (
                "<b1>Translated</b1>",
                crate::translate::PlaceholderViolation::Missing,
                1,
            ),
            (
                "<b1>Translated</b1>{v1}{v1}",
                crate::translate::PlaceholderViolation::Duplicate,
                1,
            ),
            (
                "<b1>Translated</b1>{v1}{v2}",
                crate::translate::PlaceholderViolation::Unknown,
                1,
            ),
            (
                "</b1>Translated<b1>{v1}",
                crate::translate::PlaceholderViolation::TagNesting,
                1,
            ),
            (
                "<b1>Translated</b1>{v1",
                crate::translate::PlaceholderViolation::PartialToken,
                1,
            ),
            (
                "<b1>Translated</b1>{v2}U{v1}",
                crate::translate::PlaceholderViolation::FormulaOrder,
                2,
            ),
        ] {
            let mut document = Document::for_inspection(fixture());
            let engine = FakeEngine::default();
            let translator = StaticTranslator {
                output,
                calls: AtomicUsize::new(0),
            };
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
            let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
            for character in &mut chars[..2] {
                character.font.resource_name = "BodyBold".to_owned();
            }
            let layout = chars[2].layout.as_mut().unwrap();
            layout.label = LayoutLabel::InlineFormula;
            layout.policy = TranslationPolicy::Passthrough;
            if formula_count == 2 {
                let layout = chars[4].layout.as_mut().unwrap();
                layout.label = LayoutLabel::InlineFormula;
                layout.policy = TranslationPolicy::Passthrough;
            }

            styles_and_formulas(&mut document, &context).unwrap();
            translate(&mut document, &context).unwrap();

            let paragraph = &document.il.pages[0].paragraphs[0];
            assert_eq!(
                paragraph.preserved,
                Some(il::PreservedReason::PlaceholderViolation),
                "output {output}"
            );
            assert!(paragraph.translated_text.is_none(), "output {output}");
            assert_eq!(
                translator.calls.load(Ordering::SeqCst),
                2,
                "output {output}"
            );
            let diagnostic = document
                .diagnostics
                .entries()
                .iter()
                .find(|diagnostic| {
                    matches!(
                        diagnostic,
                        Diagnostic::PlaceholderViolation {
                            page_index: 0,
                            paragraph_index: 0,
                            violation,
                        } if *violation == expected
                    )
                })
                .unwrap_or_else(|| {
                    panic!(
                        "output {output}, expected {expected:?}, diagnostics {:?}",
                        document.diagnostics.entries()
                    )
                });
            let wire =
                serde_json::to_value(crate::event::DiagnosticEvent::from(diagnostic)).unwrap();
            assert_eq!(wire["violation"], expected.wire_name(), "output {output}");
            assert!(
                document
                    .diagnostics
                    .debug_events()
                    .iter()
                    .any(|diagnostic| {
                        matches!(
                            diagnostic,
                            crate::event::DiagnosticEvent::TranslationFailureProfile {
                                page_index: 0,
                                paragraph_index: 0,
                                ..
                            }
                        )
                    })
            );
            push_degradation_summary(&mut document);
            let summary = document
                .diagnostics
                .entries()
                .iter()
                .find(|diagnostic| {
                    matches!(
                        diagnostic,
                        Diagnostic::DegradationSummary {
                            preserved_paragraphs,
                            ..
                        } if preserved_paragraphs.len() == 1
                            && preserved_paragraphs[0].placeholder_violation == Some(expected)
                    )
                })
                .unwrap();
            let wire = serde_json::to_value(crate::event::DiagnosticEvent::from(summary)).unwrap();
            assert_eq!(
                wire["preserved_paragraphs"][0]["placeholder_violation"],
                expected.wire_name(),
                "output {output}"
            );
        }
    }

    #[test]
    fn repeated_backend_echo_is_visible_without_becoming_hard_degradation() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "MIMUS",
            calls: AtomicUsize::new(0),
        };
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
        styles_and_formulas(&mut document, &context).unwrap();

        translate(&mut document, &context).unwrap();

        let paragraph = &document.il.pages[0].paragraphs[0];
        assert_eq!(paragraph.translated_text.as_deref(), Some("MIMUS"));
        assert_eq!(paragraph.preserved, None);
        assert_eq!(translator.calls.load(Ordering::SeqCst), 2);
        let diagnostics = document
            .diagnostics
            .events()
            .into_iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>();
        assert!(diagnostics.iter().any(|event| {
            event["id"] == "suspicious_echo"
                && event["page_index"] == 0
                && event["paragraph_index"] == 0
        }));

        push_degradation_summary(&mut document);
        let summary = document
            .diagnostics
            .events()
            .into_iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .find(|event| event["id"] == "degradation_summary")
            .unwrap();
        assert_eq!(summary["preserved_paragraph_count"], 0);
        assert_eq!(summary["suspicious_echo_count"], 1);
        assert_eq!(summary["suspicious_echoes"][0]["page_index"], 0);
        assert_eq!(summary["suspicious_echoes"][0]["paragraph_index"], 0);
    }

    #[test]
    fn prose_shaped_echo_rate_emits_one_document_warning() {
        const PROSE: &str = "MIMUSMIMUSMIMUSMIMUSMIMUSMIMUSMIMUSMIMUS";
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: PROSE,
            calls: AtomicUsize::new(0),
        };
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
        let paragraph = &mut document.il.pages[0].paragraphs[0];
        let original = paragraph.chars().to_vec();
        let TextCarrier::Chars { chars } = &mut paragraph.text;
        chars.clear();
        for _ in 0..8 {
            chars.extend(original.iter().cloned());
        }
        styles_and_formulas(&mut document, &context).unwrap();

        translate(&mut document, &context).unwrap();

        assert_eq!(document.diagnostics.warning_count(), 2);
        assert!(
            document
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| matches!(diagnostic, Diagnostic::SuspiciousEcho { .. }))
        );
        assert!(
            document
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| matches!(
                    diagnostic,
                    Diagnostic::SuspiciousTranslationEchoRate {
                        identity_count: 1,
                        prose_paragraph_count: 1,
                    }
                ))
        );
    }

    #[test]
    fn second_identical_run_hits_translation_and_term_caches() {
        let directory = tempfile::tempdir().unwrap();
        let cache_path = directory.path().join("translations.redb");
        let translator = GlossaryTranslator::default();
        let events = RecordingEventSink::default();
        for _ in 0..2 {
            translate_fixture_once(
                &translator,
                &events,
                crate::context::PipelineConfig {
                    cache_path: Some(cache_path.clone()),
                    ..crate::context::PipelineConfig::default()
                },
            )
            .unwrap();
        }

        assert_eq!(translator.term_calls.load(Ordering::SeqCst), 1);
        assert_eq!(translator.translation_glossaries.lock().unwrap().len(), 1);
        let statuses = events
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                EventKind::TranslationCache { status, .. } => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(statuses, [CacheStatus::Miss, CacheStatus::Hit]);
        let bytes = std::fs::read(cache_path).unwrap();
        assert!(!bytes.windows(b"MIMUS".len()).any(|part| part == b"MIMUS"));
    }

    #[test]
    fn no_cache_bypasses_reads_and_writes() {
        let directory = tempfile::tempdir().unwrap();
        let unused_path = directory.path().join("translations.redb");
        let translator = GlossaryTranslator::default();
        let events = RecordingEventSink::default();
        for _ in 0..2 {
            translate_fixture_once(
                &translator,
                &events,
                crate::context::PipelineConfig::default(),
            )
            .unwrap();
        }

        assert_eq!(translator.term_calls.load(Ordering::SeqCst), 2);
        assert_eq!(translator.translation_glossaries.lock().unwrap().len(), 2);
        assert!(!unused_path.exists());
        assert!(
            events
                .events()
                .iter()
                .all(|event| !matches!(event.kind, EventKind::TranslationCache { .. }))
        );
    }

    #[test]
    fn paragraph_translation_is_bounded_and_applied_in_reading_order() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = BoundedTranslator::new(4);
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                auto_terms: false,
                max_concurrency: 4,
                ..crate::context::PipelineConfig::default()
            },
        };
        inspect(&mut document, &context).unwrap();
        let original = document.il.pages[0].paragraphs[0].clone();
        document.il.pages[0].paragraphs = (0..8)
            .map(|index| {
                let mut paragraph = original.clone();
                paragraph.reading_order = index;
                let TextCarrier::Chars { chars } = &mut paragraph.text;
                chars[0].unicode = Some(char::from(b'A' + index as u8));
                paragraph
            })
            .collect();
        styles_and_formulas(&mut document, &context).unwrap();
        extract_terms(&mut document, &context).unwrap();

        translate(&mut document, &context).unwrap();

        assert_eq!(translator.calls.load(Ordering::SeqCst), 8);
        assert_eq!(translator.max_in_flight.load(Ordering::SeqCst), 4);
        assert_eq!(translator.in_flight.load(Ordering::SeqCst), 0);
        assert_eq!(crate::context::PipelineConfig::default().max_concurrency, 4);
        for (index, paragraph) in document.il.pages[0].paragraphs.iter().enumerate() {
            assert_eq!(paragraph.reading_order, index);
            assert_eq!(
                paragraph.translated_text.as_deref(),
                Some(format!("translated:{}IMUS", char::from(b'A' + index as u8)).as_str())
            );
        }
    }

    #[test]
    fn retry_waits_and_attempts_become_ordered_structured_diagnostics() {
        let translator = RetryingTranslator::default();
        let sleeper = Arc::new(RecordingSleeper::default());
        let events = RecordingEventSink::default();
        let document = translate_fixture_once(
            &translator,
            &events,
            crate::context::PipelineConfig {
                auto_terms: false,
                sleeper: sleeper.clone(),
                ..crate::context::PipelineConfig::default()
            },
        )
        .unwrap();

        assert_eq!(translator.calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            *sleeper.durations.lock().unwrap(),
            [
                std::time::Duration::from_millis(250),
                std::time::Duration::from_millis(500),
            ]
        );
        assert_eq!(
            document
                .diagnostics
                .entries()
                .iter()
                .filter_map(|diagnostic| match diagnostic {
                    Diagnostic::TranslationRetry {
                        page_index,
                        paragraph_index,
                        attempt,
                        delay_ms,
                        reason,
                    } => Some((*page_index, *paragraph_index, *attempt, *delay_ms, *reason,)),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [
                (0, 0, 1, 250, crate::error::RetryReason::RateLimited),
                (0, 0, 2, 500, crate::error::RetryReason::RateLimited),
            ]
        );
    }

    #[test]
    fn backend_failures_do_not_enter_cache_and_identity_outcomes_do() {
        let directory = tempfile::tempdir().unwrap();
        let cache_path = directory.path().join("translations.redb");
        let events = RecordingEventSink::default();
        let failing = FailingTranslator::default();
        let cache_config = || crate::context::PipelineConfig {
            cache_path: Some(cache_path.clone()),
            ..crate::context::PipelineConfig::default()
        };
        for _ in 0..2 {
            let document = translate_fixture_once(&failing, &events, cache_config()).unwrap();
            assert_eq!(
                document.il.pages[0].paragraphs[0].preserved,
                Some(il::PreservedReason::TranslationFailure)
            );
        }
        assert_eq!(failing.calls.load(Ordering::SeqCst), 2);

        let identity = StaticTranslator {
            output: "MIMUS",
            calls: AtomicUsize::new(0),
        };
        for _ in 0..2 {
            let document = translate_fixture_once(&identity, &events, cache_config()).unwrap();
            let paragraph = &document.il.pages[0].paragraphs[0];
            assert_eq!(paragraph.preserved, None);
            assert_eq!(paragraph.translated_text.as_deref(), Some("MIMUS"));
            assert_eq!(document.diagnostics.warning_count(), 1);
            assert!(document.diagnostics.entries().iter().any(|diagnostic| {
                matches!(diagnostic, Diagnostic::TranslationIdentity { .. })
            }));
            assert!(
                document
                    .diagnostics
                    .entries()
                    .iter()
                    .any(|diagnostic| matches!(diagnostic, Diagnostic::SuspiciousEcho { .. }))
            );
        }
        assert_eq!(identity.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn expected_email_identity_does_not_emit_identity_or_echo_diagnostics() {
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "a@b.c",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                auto_terms: false,
                ..crate::context::PipelineConfig::default()
            },
        };
        let mut document = Document::for_inspection(fixture());
        inspect(&mut document, &context).unwrap();
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        for (character, unicode) in chars.iter_mut().zip("a@b.c".chars()) {
            character.unicode = Some(unicode);
        }
        styles_and_formulas(&mut document, &context).unwrap();
        extract_terms(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();

        assert_eq!(translator.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            document.il.pages[0].paragraphs[0]
                .translated_text
                .as_deref(),
            Some("a@b.c")
        );
        assert!(!document.diagnostics.entries().iter().any(|diagnostic| {
            matches!(
                diagnostic,
                Diagnostic::TranslationIdentity { .. } | Diagnostic::SuspiciousEcho { .. }
            )
        }));
    }

    #[test]
    fn failed_term_extraction_never_enters_cache() {
        let directory = tempfile::tempdir().unwrap();
        let cache_path = directory.path().join("translations.redb");
        let translator = FailingTermTranslator::default();
        let events = RecordingEventSink::default();
        for _ in 0..2 {
            assert!(
                translate_fixture_once(
                    &translator,
                    &events,
                    crate::context::PipelineConfig {
                        cache_path: Some(cache_path.clone()),
                        ..crate::context::PipelineConfig::default()
                    },
                )
                .is_err()
            );
        }

        assert_eq!(translator.term_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn normal_mode_preserves_a_failed_paragraph_and_publishes_the_source_document() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("normal.pdf");
        let mut document = Document::new(fixture(), &output);
        let engine = FakeEngine::default();
        let translator = FailingTranslator::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                auto_terms: false,
                ..crate::context::PipelineConfig::default()
            },
        };

        let result = run(&mut document, &context).unwrap();

        assert_eq!(translator.calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.warnings, 1);
        assert_eq!(
            document.il.pages[0].paragraphs[0].preserved,
            Some(il::PreservedReason::TranslationFailure)
        );
        assert_eq!(
            std::fs::read(output).unwrap(),
            std::fs::read(fixture()).unwrap()
        );
        assert!(
            document
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| matches!(
                    diagnostic,
                    Diagnostic::DegradationSummary {
                        preserved_paragraphs,
                        ..
                    } if preserved_paragraphs == &[PreservedParagraph {
                        page_index: 0,
                        paragraph_index: 0,
                        reason: il::PreservedReason::TranslationFailure,
                        placeholder_violation: None,
                    }]
                ))
        );
    }

    #[test]
    fn strict_mode_reports_all_degradation_before_publish_and_preserves_destination() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("strict.pdf");
        std::fs::write(&output, b"existing destination").unwrap();
        let mut document = Document::new(fixture(), &output);
        let engine = FakeEngine::default();
        let translator = FailingTranslator::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                auto_terms: false,
                strict: true,
                ..crate::context::PipelineConfig::default()
            },
        };

        let error = run(&mut document, &context).unwrap_err();

        assert_eq!(
            error.reason(),
            ErrorReason::Translation(TranslationReason::StrictDegradation)
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"existing destination");
        assert!(document.write_report.is_none());
        assert!(document.rewrites.is_empty());
        assert!(
            document
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| matches!(
                    diagnostic,
                    Diagnostic::DegradationSummary {
                        preserved_paragraphs,
                        ..
                    } if preserved_paragraphs == &[PreservedParagraph {
                        page_index: 0,
                        paragraph_index: 0,
                        reason: il::PreservedReason::TranslationFailure,
                        placeholder_violation: None,
                    }]
                ))
        );
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            1,
            "strict mode must not leave an output temporary"
        );
    }

    #[test]
    fn strict_mode_accumulates_degradation_across_recoverable_stages() {
        fn degrade_page(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
            document.extracted_pages[0].degraded = Some(PageDegradeReason::BadFormBBox);
            Ok(())
        }

        fn preserve_paragraph(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
            let paragraph = &mut document.il.pages[0].paragraphs[0];
            paragraph.translated_text = None;
            paragraph.preserved = Some(il::PreservedReason::TypesetOverflow);
            Ok(())
        }

        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let inspect_context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &CountingTranslator::default(),
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        inspect(&mut document, &inspect_context).unwrap();
        let strict_context = PassContext {
            config: crate::context::PipelineConfig {
                strict: true,
                ..crate::context::PipelineConfig::default()
            },
            ..inspect_context
        };

        let error = run_stages(
            &mut document,
            &strict_context,
            &[
                (Stage::ScanDetect, degrade_page),
                (Stage::Typeset, preserve_paragraph),
            ],
        )
        .unwrap_err();

        assert_eq!(
            error.reason(),
            ErrorReason::Translation(TranslationReason::StrictDegradation)
        );
        assert!(document.diagnostics.entries().iter().any(|diagnostic| {
            matches!(
                diagnostic,
                Diagnostic::DegradationSummary {
                    degraded_page_indices,
                    preserved_paragraphs,
                    ..
                } if degraded_page_indices == &[0]
                    && preserved_paragraphs == &[PreservedParagraph {
                        page_index: 0,
                        paragraph_index: 0,
                        reason: il::PreservedReason::TypesetOverflow,
                        placeholder_violation: None,
                    }]
            )
        }));
    }

    #[test]
    fn strict_mode_accepts_identity_translation_outcomes() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("strict-identity.pdf");
        let mut document = Document::new(fixture(), &output);
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "MIMUS",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                auto_terms: false,
                strict: true,
                ..crate::context::PipelineConfig::default()
            },
        };

        let result = run(&mut document, &context).unwrap();

        assert_eq!(translator.calls.load(Ordering::SeqCst), 2);
        assert_eq!(result.warnings, 2);
        assert_eq!(document.il.pages[0].paragraphs[0].preserved, None);
        assert_eq!(document.suspicious_echoes, BTreeSet::from([(0, 0)]));
        assert!(output.is_file());
    }

    #[test]
    fn strict_placeholder_failure_reports_the_exact_subtype() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = StaticTranslator {
            output: "<b1>Translated</b1>",
            calls: AtomicUsize::new(0),
        };
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                auto_terms: false,
                strict: true,
                ..crate::context::PipelineConfig::default()
            },
        };
        inspect(&mut document, &context).unwrap();
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        for character in &mut chars[..2] {
            character.font.resource_name = "BodyBold".to_owned();
        }
        let layout = chars[2].layout.as_mut().unwrap();
        layout.label = LayoutLabel::InlineFormula;
        layout.policy = TranslationPolicy::Passthrough;
        styles_and_formulas(&mut document, &context).unwrap();

        let error =
            run_stages(&mut document, &context, &[(Stage::Translate, translate)]).unwrap_err();

        assert_eq!(
            error.reason(),
            ErrorReason::Translation(TranslationReason::StrictDegradation)
        );
        assert!(
            error.to_string().contains("page 1 paragraph 1 missing"),
            "{error}"
        );
        assert!(document.diagnostics.entries().iter().any(|diagnostic| {
            matches!(
                diagnostic,
                Diagnostic::DegradationSummary {
                    preserved_paragraphs,
                    ..
                } if preserved_paragraphs == &[PreservedParagraph {
                    page_index: 0,
                    paragraph_index: 0,
                    reason: il::PreservedReason::PlaceholderViolation,
                    placeholder_violation: Some(
                        crate::translate::PlaceholderViolation::Missing,
                    ),
                }]
            )
        }));
    }

    #[test]
    fn extract_terms_calls_once_merges_user_override_dumps_and_injects_the_final_glossary() {
        let directory = tempfile::tempdir().unwrap();
        let dump = directory.path().join("final-glossary.toml");
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = GlossaryTranslator::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                user_glossary: crate::translate::Glossary::from_toml(
                    "version = 1\n[[terms]]\nsource = 'attention'\ntarget = 'user-attention'\n[[terms]]\nsource = 'cache'\ntarget = 'user-cache'\n",
                )
                .unwrap(),
                dump_glossary: Some(dump.clone()),
                ..crate::context::PipelineConfig::default()
            },
        };
        inspect(&mut document, &context).unwrap();
        styles_and_formulas(&mut document, &context).unwrap();
        extract_terms(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();

        assert_eq!(translator.term_calls.load(Ordering::SeqCst), 1);
        let final_glossary = &translator.translation_glossaries.lock().unwrap()[0];
        assert_eq!(final_glossary.entries()["attention"], "user-attention");
        assert_eq!(final_glossary.entries()["model"], "auto-model");
        assert_eq!(final_glossary.entries()["cache"], "user-cache");
        assert_eq!(
            crate::translate::Glossary::from_path(&dump).unwrap(),
            *final_glossary
        );
    }

    #[test]
    fn no_auto_terms_skips_extraction_entirely() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = GlossaryTranslator::default();
        let events = RecordingEventSink::default();
        let user_glossary = crate::translate::Glossary::from_toml(
            "version = 1\n[[terms]]\nsource = 'attention'\ntarget = 'user-attention'\n",
        )
        .unwrap();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                user_glossary: user_glossary.clone(),
                auto_terms: false,
                ..crate::context::PipelineConfig::default()
            },
        };
        inspect(&mut document, &context).unwrap();
        styles_and_formulas(&mut document, &context).unwrap();
        extract_terms(&mut document, &context).unwrap();

        assert_eq!(translator.term_calls.load(Ordering::SeqCst), 0);
        assert_eq!(document.glossary, user_glossary);
    }

    #[test]
    fn automatic_terms_skip_empty_passthrough_documents() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = GlossaryTranslator::default();
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
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        for character in chars {
            character.layout.as_mut().unwrap().policy = TranslationPolicy::Passthrough;
        }
        styles_and_formulas(&mut document, &context).unwrap();
        assert!(
            document
                .prepared_translations
                .values()
                .all(|prepared| prepared.request_text().is_empty())
        );

        extract_terms(&mut document, &context).unwrap();

        assert_eq!(translator.term_calls.load(Ordering::SeqCst), 0);
        assert!(document.glossary.is_empty());
    }

    #[test]
    fn whitespace_and_numeric_only_requests_are_local_identity_without_backend_or_ink() {
        for source in ["     ", "2. 3 "] {
            let mut document = Document::for_inspection(fixture());
            let engine = FakeEngine::default();
            let translator = GlossaryTranslator::default();
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
            let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
            for (character, unicode) in chars.iter_mut().zip(source.chars()) {
                character.unicode = Some(unicode);
            }

            styles_and_formulas(&mut document, &context).unwrap();
            extract_terms(&mut document, &context).unwrap();
            translate(&mut document, &context).unwrap();
            typeset(&mut document, &context).unwrap();

            assert_eq!(
                translator.term_calls.load(Ordering::SeqCst),
                0,
                "{source:?}"
            );
            assert!(
                translator.translation_glossaries.lock().unwrap().is_empty(),
                "{source:?}"
            );
            assert_eq!(
                document.il.pages[0].paragraphs[0]
                    .translated_text
                    .as_deref(),
                Some(source),
                "{source:?}"
            );
            assert!(
                document
                    .rewrites
                    .iter()
                    .all(|rewrite| rewrite.typeset_ink_bounds.is_empty()),
                "{source:?}"
            );
        }
    }

    #[test]
    fn numeric_only_requests_skip_the_none_backend_and_create_no_ink() {
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
            config: crate::context::PipelineConfig {
                auto_terms: false,
                ..crate::context::PipelineConfig::default()
            },
        };
        inspect(&mut document, &context).unwrap();
        let TextCarrier::Chars { chars } = &mut document.il.pages[0].paragraphs[0].text;
        for (character, unicode) in chars.iter_mut().zip("2. 3 ".chars()) {
            character.unicode = Some(unicode);
        }

        styles_and_formulas(&mut document, &context).unwrap();
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();

        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            document.il.pages[0].paragraphs[0]
                .translated_text
                .as_deref(),
            Some("2. 3 ")
        );
        assert!(
            document
                .rewrites
                .iter()
                .all(|rewrite| rewrite.typeset_ink_bounds.is_empty())
        );
    }

    #[test]
    fn invalid_page_rotation_degrades_and_preserves_the_input() {
        let directory = tempfile::tempdir().unwrap();
        let input_path = directory.path().join("rotate-45.pdf");
        let output_path = directory.path().join("rotate-45-output.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        pdf.get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Rotate", 45);
        pdf.save(&input_path).unwrap();
        let input = std::fs::read(&input_path).unwrap();
        let mut document = Document::new(&input_path, &output_path);
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

        assert_eq!(std::fs::read(output_path).unwrap(), input);
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert!(document.rewrites.is_empty());
        assert!(document.diagnostics.entries().iter().any(|diagnostic| {
            matches!(
                diagnostic,
                Diagnostic::PageDegraded {
                    page_index: 0,
                    reason: PageDegradeReason::UnsupportedRotation,
                }
            )
        }));
    }

    #[test]
    fn singular_form_preserves_the_walker_authoritative_paragraph() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("singular-form-output.pdf");
        let mut document = Document::new(singular_form_fixture(), &output);
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

        let paragraph = &document.il.pages[0].paragraphs[0];
        assert_eq!(paragraph.source_text(), "MMIMUS");
        assert_eq!(
            paragraph.preserved,
            Some(il::PreservedReason::UnreliableUnicode)
        );
        assert!(paragraph.translated_text.is_none());
        assert!(
            paragraph
                .chars()
                .iter()
                .all(|character| character.visual_bbox == character.r#box)
        );
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert!(document.rewrites.is_empty());
        assert_eq!(
            std::fs::read(&output).unwrap(),
            std::fs::read(singular_form_fixture()).unwrap()
        );
        assert!(
            document
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| matches!(
                    diagnostic,
                    Diagnostic::DegradationSummary {
                        preserved_paragraphs,
                        ..
                    } if preserved_paragraphs == &[PreservedParagraph {
                        page_index: 0,
                        paragraph_index: 0,
                        reason: il::PreservedReason::UnreliableUnicode,
                        placeholder_violation: None,
                    }]
                ))
        );
    }

    #[test]
    fn font_embed_rejects_rewrites_without_any_font_provenance() {
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
            embedded_fonts: Vec::new(),
            typeset_characters: Vec::new(),
            typeset_ink_bounds: Vec::new(),
        }];
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
                preserved_paragraph_count: 0,
                ..
            } if degraded_page_indices == &[0] && preserved_paragraphs.is_empty()
        )));
    }

    #[test]
    fn one_backend_failure_preserves_only_its_own_paragraph() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &SelectiveFailTranslator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig {
                auto_terms: false,
                ..crate::context::PipelineConfig::default()
            },
        };
        inspect(&mut document, &context).unwrap();
        let original = document.il.pages[0].paragraphs[0].clone();
        document.il.pages[0].paragraphs = (0..3)
            .map(|index| {
                let mut paragraph = original.clone();
                paragraph.reading_order = index;
                let TextCarrier::Chars { chars } = &mut paragraph.text;
                chars[0].unicode = Some(char::from(b'A' + index as u8));
                paragraph
            })
            .collect();
        styles_and_formulas(&mut document, &context).unwrap();
        extract_terms(&mut document, &context).unwrap();

        translate(&mut document, &context).unwrap();

        let paragraphs = &document.il.pages[0].paragraphs;
        assert_eq!(
            paragraphs[0].translated_text.as_deref(),
            Some("translated:AIMUS")
        );
        assert_eq!(paragraphs[0].preserved, None);
        assert_eq!(paragraphs[1].translated_text, None);
        assert_eq!(
            paragraphs[1].preserved,
            Some(il::PreservedReason::TranslationFailure)
        );
        assert_eq!(
            paragraphs[2].translated_text.as_deref(),
            Some("translated:CIMUS")
        );
        assert_eq!(paragraphs[2].preserved, None);
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
                        placeholder_violation: None,
                    }]
        )));
    }
}
