use std::collections::{BTreeMap, BTreeSet, HashMap};

use lopdf::{Document as LopdfDocument, Object, ObjectId};

use crate::context::{CharacterAlignment, Document, ExtractedPage, OutputFont, PassContext};
use crate::engine::{PageCharSnapshot, RgbaImage};
use crate::error::{AssetReason, InputReason, InternalReason, IoReason, MimusError, Result};
use crate::event::{
    Diagnostic, Diagnostics, Event, EventKind, PageDegradeReason, PreservedParagraph, RecoveryKind,
    Stage,
};
use crate::geometry::{PageFrame, PageGeometryResolveError};
use crate::il::{
    self, Char, LayoutAssignment, LayoutLabel, LayoutSource, PageGeometry, Paragraph,
    PassthroughRef, Rect, TextCarrier, TextTransform, TranslationPolicy,
};
use crate::scan::{PageClass, prescan_page};
#[cfg(test)]
use crate::walk::walk_page;
use crate::walk::{PageWalkError, UnicodeProvenance, walk_page_detailed_with_rotation};
use crate::write::{
    ContentSpanReplacement, EmbeddedFont, PageRewrite, TypesetCharacter, build_incremental, publish,
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
    let placeholder_violations = &document.placeholder_violations;
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
                    integers.chunks_exact(2).all(|pair| {
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
                    document.diagnostics.push(Diagnostic::ContentRecovered {
                        page_index: page.index,
                        recovery,
                        form_cycle_paths,
                    });
                }
                page.walked_characters = walked.characters;
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
            let raster = context
                .engine
                .rasterize_page(&document.original_bytes, page.index)?;
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
        if region.source == LayoutSource::Model && top_ratio >= 0.88 {
            region.label = LayoutLabel::Header;
        } else if region.source == LayoutSource::Model && bottom_ratio <= 0.12 {
            region.label = LayoutLabel::Footer;
        } else if looks_like_reference_entry(trimmed) {
            region.label = LayoutLabel::ReferenceContent;
        } else if region.bounds.top < geometry.height * 0.5 && looks_like_seal(trimmed) {
            region.label = LayoutLabel::Seal;
        }
    }
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

fn layout_assignment(
    regions: &[crate::engine::LayoutRegion],
    bounds: Rect,
) -> Option<LayoutAssignment> {
    let char_area = rect_area(bounds);
    regions
        .iter()
        .filter_map(|region| {
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
            Some((region, coverage))
        })
        .max_by(|(left, left_coverage), (right, right_coverage)| {
            layout_assignment_priority(left, regions, bounds)
                .cmp(&layout_assignment_priority(right, regions, bounds))
                .then_with(|| left_coverage.total_cmp(right_coverage))
                .then_with(|| rect_area(right.bounds).total_cmp(&rect_area(left.bounds)))
                .then_with(|| left.confidence.total_cmp(&right.confidence))
                .then_with(|| right.reading_order.cmp(&left.reading_order))
        })
        .map(|(region, _)| LayoutAssignment {
            label: region.label,
            reading_order: region.reading_order,
            bounds: region.bounds,
            source: region.source,
            policy: region.label.translation_policy(),
        })
}

fn layout_assignment_priority(
    region: &crate::engine::LayoutRegion,
    regions: &[crate::engine::LayoutRegion],
    bounds: Rect,
) -> u8 {
    match region.source {
        LayoutSource::FallbackLine => 1,
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
        if extracted.walked_characters.is_empty() && extracted.layout_regions.is_empty() {
            pages.push(il::Page {
                index: extracted.index,
                geometry: extracted.geometry,
                paragraphs: Vec::new(),
            });
            continue;
        }
        let positioned = extracted
            .walked_characters
            .iter()
            .enumerate()
            .map(|(index, walked)| PositionedChar {
                walked_index: index,
                locatable: walked.locatable,
                force_no_space_before: false,
                character: Char {
                    unicode: walked.unicode,
                    code: walked.code,
                    visible: walked.visible,
                    font: walked.font.clone(),
                    font_size: walked.font_size,
                    baseline_origin: walked.baseline_origin,
                    r#box: walked.metric_box,
                    visual_bbox: extracted
                        .character_alignment
                        .engine_indices_by_walk
                        .get(index)
                        .and_then(|engine_index| *engine_index)
                        .and_then(|engine_index| extracted.engine_characters.get(engine_index))
                        .filter(|_| walked.locatable)
                        .map_or(walked.metric_box, |engine| engine.tight_box),
                    text_transform: walked.text_transform,
                    implicit_space_before: false,
                    layout: layout_assignment(&extracted.layout_regions, walked.metric_box),
                    passthrough: PassthroughRef {
                        content_object: walked.content_object.0,
                        byte_start: walked.byte_start,
                        byte_end: walked.byte_end,
                        encoded: walked.encoded.clone(),
                    },
                },
            })
            .collect::<Vec<_>>();

        let mut model_groups = Vec::<ModelGroup>::new();
        let mut fallback = Vec::new();
        let mut isolated = Vec::new();
        for positioned in positioned {
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

        model_groups.sort_by_key(|group| group.assignment.reading_order);
        let mut drafts = Vec::new();
        for mut group in model_groups {
            if group.assignment.label == LayoutLabel::AsideText
                && chars_are_narrow_number(&group.chars)
            {
                mark_chars_as_number(&mut group.chars);
            }
            let mut lines = build_text_lines(group.chars);
            merge_toc_page_numbers(&mut lines);
            for paragraph_lines in split_natural_paragraphs(lines, group.assignment.label) {
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
        let paragraphs = drafts
            .into_iter()
            .enumerate()
            .map(|(reading_order, draft)| {
                paragraph_from_lines(
                    reading_order,
                    draft.lines,
                    &extracted.walked_characters,
                    &extracted.character_alignment.weak_unicode_conflicts,
                )
            })
            .collect();
        pages.push(il::Page {
            index: extracted.index,
            geometry: extracted.geometry,
            paragraphs,
        });
    }
    document.il = il::Document {
        schema_version: il::SCHEMA_VERSION,
        pages,
    };
    Ok(())
}

fn build_text_lines(mut chars: Vec<PositionedChar>) -> Vec<TextLine> {
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
                gap > font_size * 1.8 || (leading_number && gap > font_size * 0.8)
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
) -> Paragraph {
    let bounds = lines_bounds(&lines);
    let mut positioned = Vec::new();
    for line in lines {
        positioned.extend(line.chars);
    }
    let preserved = paragraph_preserved_reason(
        positioned
            .iter()
            .map(|positioned| (positioned.walked_index, &walked[positioned.walked_index])),
        weak_unicode_conflicts,
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
        text: TextCarrier::Chars { chars },
        translated_text: None,
        preserved,
    }
}

pub fn styles_and_formulas(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    prepare_translations(document)?;
    Ok(())
}

pub fn extract_terms(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let document_text = document
        .prepared_translations
        .values()
        .map(crate::translate::PreparedTranslation::request_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let automatic = if context.config.auto_terms
        && context.translator.model_id() != "none"
        && !document_text.is_empty()
    {
        context
            .translator
            .extract_terms(&crate::translate::TermExtractionRequest {
                document_text: &document_text,
                target_language: &context.config.target_language,
            })?
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
    if document.prepared_translations.is_empty() {
        prepare_translations(document)?;
    }
    document.placeholder_violations.clear();
    let mut prose_paragraph_count = 0;
    let mut prose_identity_count = 0;
    for page in &mut document.il.pages {
        let content_objects = page_content_objects.get(page.index).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                format!("Translate could not find extracted page {}", page.index),
            )
        })?;
        for (paragraph_index, paragraph) in page.paragraphs.iter_mut().enumerate() {
            if paragraph.preserved.is_some() {
                paragraph.translated_text = None;
                continue;
            }
            if context.translator.model_id() != "none" {
                let key = (page.index, paragraph.reading_order);
                let prepared = document.prepared_translations.get(&key).ok_or_else(|| {
                    MimusError::internal(
                        InternalReason::InvariantViolation,
                        format!("Translate could not find prepared paragraph {key:?}"),
                    )
                })?;
                if prepared.request_text().is_empty() {
                    paragraph.translated_text = Some(paragraph.source_text());
                    continue;
                }
                let prose_shaped = translation_request_is_prose_shaped(prepared.request_text());
                prose_paragraph_count += usize::from(prose_shaped);
                let output =
                    context
                        .translator
                        .translate(&crate::translate::TranslationRequest {
                            text: prepared.request_text(),
                            target_language: &context.config.target_language,
                            glossary: &document.glossary,
                        })?;
                match prepared.classify(&output) {
                    crate::translate::TranslationOutcome::Identity => {
                        paragraph.translated_text = Some(paragraph.source_text());
                        prose_identity_count += usize::from(prose_shaped);
                        document.diagnostics.push(Diagnostic::TranslationIdentity {
                            page_index: page.index,
                            paragraph_index,
                            request_characters: prepared.request_text().chars().count(),
                        });
                    }
                    crate::translate::TranslationOutcome::Translated(validated) => {
                        match prepared.restore(&validated) {
                            Ok(restored) => {
                                paragraph.translated_text = Some(restored.plain_text());
                                document.restored_translations.insert(key, restored);
                            }
                            Err(violation) => preserve_placeholder_violation(
                                paragraph,
                                &mut document.diagnostics,
                                &mut document.placeholder_violations,
                                page.index,
                                paragraph_index,
                                violation,
                                &output,
                            ),
                        }
                    }
                    crate::translate::TranslationOutcome::PlaceholderViolation(violation) => {
                        preserve_placeholder_violation(
                            paragraph,
                            &mut document.diagnostics,
                            &mut document.placeholder_violations,
                            page.index,
                            paragraph_index,
                            violation,
                            &output,
                        );
                    }
                }
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
    output: &str,
) {
    paragraph.translated_text = None;
    paragraph.preserved = Some(il::PreservedReason::PlaceholderViolation);
    placeholder_violations.insert((page_index, paragraph_index), violation);
    diagnostics.push(Diagnostic::PlaceholderViolation {
        page_index,
        paragraph_index,
        violation,
    });
    let profile = crate::translate::redacted_translation_profile(output);
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
    let mut prepared = BTreeMap::new();
    for page in &document.il.pages {
        let content_objects = page_content_objects.get(page.index).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                format!(
                    "StylesAndFormulas could not find extracted page {}",
                    page.index
                ),
            )
        })?;
        for paragraph in &page.paragraphs {
            if paragraph.preserved.is_some() {
                continue;
            }
            let chars = paragraph.chars();
            let mut parts = Vec::new();
            let mut start = 0;
            while start < chars.len() {
                let class = prepared_character_class(&chars[start], content_objects);
                let mut end = start + 1;
                while end < chars.len()
                    && prepared_character_class(&chars[end], content_objects) == class
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
    Ok(())
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
            .contains("bold");
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
        let mut replacements = BTreeMap::<(lopdf::ObjectId, usize, usize), Vec<u8>>::new();
        let mut reused_fonts = BTreeSet::new();
        let mut plans = Vec::new();
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
                for character in chars {
                    let Some(content_object) = unique_page_content(character, &content_objects)?
                    else {
                        continue;
                    };
                    if !character.visible || character.text_transform != TextTransform::Upright {
                        continue;
                    }
                    let key = span_key(character, content_object);
                    let bytes = streams[&content_object].get(key.1..key.2).ok_or_else(|| {
                        span_out_of_bounds(
                            content_object,
                            key.1,
                            key.2,
                            streams[&content_object].len(),
                        )
                    })?;
                    replacements.entry(key).or_insert_with(|| bytes.to_vec());
                    reused_fonts.insert(character.font.clone());
                }
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
            match plan_paragraph(
                paragraph,
                translated,
                restored,
                &content_objects,
                output_fonts,
            ) {
                Ok(paragraph_plans) => plans.extend(paragraph_plans),
                Err(TypesetPlanError::Preserved(reason)) => {
                    preserved.push((page.index, paragraph.reading_order, reason));
                }
                Err(TypesetPlanError::MissingGlyphs {
                    missing_characters,
                    font,
                }) => {
                    document
                        .diagnostics
                        .push(Diagnostic::UnsupportedOutputGlyph {
                            page_index: page.index,
                            reading_order: paragraph.reading_order,
                            missing_characters,
                            font_source: font.source.clone(),
                            font_sha256: font.sha256.clone(),
                        });
                    preserved.push((
                        page.index,
                        paragraph.reading_order,
                        il::PreservedReason::UnsupportedFont,
                    ));
                }
            }
        }

        let mut output_fonts = BTreeMap::new();
        let mut typeset_characters = Vec::new();
        for bold in [false, true] {
            let used = plans
                .iter()
                .flat_map(|plan| plan.lines.iter().flatten())
                .filter(|character| character.bold == bold)
                .map(|character| character.value)
                .collect::<BTreeSet<_>>();
            if used.is_empty() {
                continue;
            }
            let source_font = if bold {
                &context.config.output_fonts.as_ref().unwrap().bold
            } else {
                &context.config.output_fonts.as_ref().unwrap().regular
            };
            let (font, cids) = build_embedded_font(&used, source_font, bold).map_err(|_| {
                MimusError::internal(
                    InternalReason::InvariantViolation,
                    "validated output font could not be subset for translated text",
                )
            })?;
            output_fonts.insert(bold, BuiltOutputFont { font, cids });
        }
        for plan in &plans {
            install_typeset_replacements(plan, &output_fonts, &mut replacements)?;
            typeset_characters.extend(planned_characters(plan, &output_fonts));
        }
        let embedded_fonts = output_fonts
            .into_values()
            .map(|output| output.font)
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
    document.rewrites = rewrites;
    Ok(())
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

#[derive(Debug)]
struct TypesetPlan {
    spans: Vec<SpanKey>,
    lines: Vec<Vec<crate::translate::StyledCharacter>>,
    baselines: Vec<(f64, f64)>,
    font_size: f64,
}

struct BuiltOutputFont {
    font: EmbeddedFont,
    cids: BTreeMap<char, u16>,
}

const MIN_FONT_SIZE_PT: f64 = 8.0;
const LINE_ADVANCE_EM: f64 = 1.5;

enum TypesetPlanError<'a> {
    Preserved(il::PreservedReason),
    MissingGlyphs {
        missing_characters: String,
        font: &'a OutputFont,
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

fn plan_paragraph<'a>(
    paragraph: &Paragraph,
    translated: &str,
    restored: Option<&crate::translate::RestoredTranslation>,
    content_objects: &BTreeSet<lopdf::ObjectId>,
    output_fonts: &'a crate::context::OutputFonts,
) -> std::result::Result<Vec<TypesetPlan>, TypesetPlanError<'a>> {
    let all_chars = paragraph.chars();
    let content_object_numbers = content_objects
        .iter()
        .map(|id| id.0)
        .collect::<BTreeSet<_>>();
    let source_segments = source_text_segments(all_chars, &content_object_numbers);
    let translated_segments = if let Some(restored) = restored {
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
    source_segments
        .into_iter()
        .zip(translated_segments)
        .filter(|(source, translated)| !source.is_empty() || !translated.is_empty())
        .map(|(source, translated)| {
            plan_text_segment(&source, &translated, content_objects, output_fonts)
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

fn plan_text_segment<'a>(
    chars: &[&Char],
    translated: &[crate::translate::StyledCharacter],
    content_objects: &BTreeSet<lopdf::ObjectId>,
    output_fonts: &'a crate::context::OutputFonts,
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
    if translated.is_empty() {
        return Ok(TypesetPlan {
            spans,
            lines: Vec::new(),
            baselines: Vec::new(),
            font_size: chars[0].font_size.max(MIN_FONT_SIZE_PT),
        });
    }
    let regular = ttf_parser::Face::parse(&output_fonts.regular.bytes, 0)
        .map_err(|_| TypesetPlanError::Preserved(il::PreservedReason::UnsupportedFont))?;
    let bold = ttf_parser::Face::parse(&output_fonts.bold.bytes, 0)
        .map_err(|_| TypesetPlanError::Preserved(il::PreservedReason::UnsupportedFont))?;
    for (is_bold, face, font) in [
        (false, &regular, &output_fonts.regular),
        (true, &bold, &output_fonts.bold),
    ] {
        let missing_characters = translated
            .iter()
            .filter(|character| character.bold == is_bold)
            .map(|character| character.value)
            .filter(|value| face.glyph_index(*value).is_none())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(16)
            .collect::<String>();
        if !missing_characters.is_empty() {
            return Err(TypesetPlanError::MissingGlyphs {
                missing_characters,
                font,
            });
        }
    }
    let container = chars
        .iter()
        .filter_map(|character| character.layout.map(|layout| layout.bounds))
        .reduce(Rect::union)
        .ok_or(TypesetPlanError::Preserved(
            il::PreservedReason::TypesetOverflow,
        ))?;
    let preferred = chars
        .iter()
        .map(|character| character.font_size)
        .sum::<f64>()
        / chars.len() as f64;
    let first = chars[0];
    let mut size = preferred.max(MIN_FONT_SIZE_PT);
    while size + 0.001 >= MIN_FONT_SIZE_PT {
        if let Some(lines) = wrap_styled_text(
            translated,
            &regular,
            &bold,
            size,
            container.right - container.left,
        ) {
            let ascent = f64::from(regular.ascender().max(bold.ascender()))
                / f64::from(regular.units_per_em())
                * size;
            let descent = f64::from(regular.descender().min(bold.descender()))
                / f64::from(regular.units_per_em())
                * size;
            let first_y = first.baseline_origin.y.min(container.top - ascent);
            let baselines = lines
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    (
                        container.left,
                        first_y - index as f64 * size * LINE_ADVANCE_EM,
                    )
                })
                .collect::<Vec<_>>();
            let last_y = baselines.last().unwrap().1;
            if last_y + descent >= container.bottom - 0.01 {
                return Ok(TypesetPlan {
                    spans,
                    lines,
                    baselines,
                    font_size: size,
                });
            }
        }
        size -= 0.5;
    }
    Err(TypesetPlanError::Preserved(
        il::PreservedReason::TypesetOverflow,
    ))
}

fn wrap_styled_text(
    text: &[crate::translate::StyledCharacter],
    regular: &ttf_parser::Face<'_>,
    bold: &ttf_parser::Face<'_>,
    size: f64,
    width: f64,
) -> Option<Vec<Vec<crate::translate::StyledCharacter>>> {
    let mut lines = vec![Vec::new()];
    let mut line_width = 0.0;
    for token in styled_text_tokens(text) {
        let token_width = token.iter().try_fold(0.0, |sum, character| {
            let face = if character.bold { bold } else { regular };
            let glyph = face.glyph_index(character.value)?;
            Some(
                sum + f64::from(face.glyph_hor_advance(glyph)?) / f64::from(face.units_per_em())
                    * size,
            )
        })?;
        if token_width > width + 0.01 {
            return None;
        }
        if line_width > 0.0 && line_width + token_width > width + 0.01 {
            lines.push(Vec::new());
            line_width = 0.0;
        }
        lines.last_mut().unwrap().extend(token);
        line_width += token_width;
    }
    Some(lines)
}

fn styled_text_tokens(
    text: &[crate::translate::StyledCharacter],
) -> Vec<Vec<crate::translate::StyledCharacter>> {
    let mut tokens = Vec::<Vec<crate::translate::StyledCharacter>>::new();
    for &character in text {
        let joins_ascii_word = character.value.is_ascii_alphanumeric()
            && tokens
                .last()
                .and_then(|token| token.last())
                .is_some_and(|previous| previous.value.is_ascii_alphanumeric());
        if joins_ascii_word {
            tokens.last_mut().unwrap().push(character);
        } else {
            tokens.push(vec![character]);
        }
    }
    tokens
}

fn build_embedded_font(
    used: &BTreeSet<char>,
    source_font: &OutputFont,
    bold: bool,
) -> std::result::Result<(EmbeddedFont, BTreeMap<char, u16>), ()> {
    let bytes = &source_font.bytes;
    let face = ttf_parser::Face::parse(bytes, 0).map_err(|_| ())?;
    let mut remapper = subsetter::GlyphRemapper::new();
    let mut original = Vec::new();
    for character in used {
        let glyph = face.glyph_index(*character).ok_or(())?;
        remapper.remap(glyph.0);
        original.push((*character, glyph));
    }
    let font_bytes = subsetter::subset(bytes, 0, &remapper).map_err(|_| ())?;
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
    let weight = if bold { "Bold" } else { "Regular" };
    let tag = subset_tag(used, bold);
    Ok((
        EmbeddedFont {
            resource_name: if bold { "MimusB" } else { "MimusR" }.to_owned(),
            base_font: format!("{tag}+{}-{weight}", source_font.postscript_name),
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

fn subset_tag(used: &BTreeSet<char>, bold: bool) -> String {
    let mut hash = if bold { 0x811c9dc4u32 } else { 0x811c9dc5u32 };
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
    fonts: &BTreeMap<bool, BuiltOutputFont>,
    replacements: &mut BTreeMap<SpanKey, Vec<u8>>,
) -> Result<()> {
    if plan.lines.is_empty() {
        replacements.insert(plan.spans[0], Vec::new());
        for span in &plan.spans[1..] {
            replacements.insert(*span, Vec::new());
        }
        return Ok(());
    }
    let mut command = String::new();
    let mut emitted_run = false;
    for (index, line) in plan.lines.iter().enumerate() {
        let (x, y) = plan.baselines[index];
        if emitted_run {
            command.push_str(" Tj\n");
        }
        command.push_str(&format!("1 0 0 1 {} {} Tm ", pdf_number(x), pdf_number(y)));
        let mut run_start = 0;
        while run_start < line.len() {
            let bold = line[run_start].bold;
            let mut run_end = run_start + 1;
            while run_end < line.len() && line[run_end].bold == bold {
                run_end += 1;
            }
            if emitted_run && run_start > 0 {
                command.push_str(" Tj ");
            }
            let output_font = fonts.get(&bold).ok_or_else(|| {
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
    replacements.insert(plan.spans[0], command.into_bytes());
    for span in &plan.spans[1..] {
        replacements.insert(*span, Vec::new());
    }
    Ok(())
}

fn planned_characters(
    plan: &TypesetPlan,
    fonts: &BTreeMap<bool, BuiltOutputFont>,
) -> Vec<TypesetCharacter> {
    let mut output = Vec::new();
    for (line, &(start_x, baseline_y)) in plan.lines.iter().zip(&plan.baselines) {
        let mut x = start_x;
        for character in line {
            let font = &fonts[&character.bold].font;
            let advance = font
                .glyphs
                .iter()
                .find_map(|(_, value, advance)| (*value == character.value).then_some(*advance))
                .expect("typeset glyph exists in its embedded font");
            output.push(TypesetCharacter {
                unicode: character.value,
                baseline_origin: il::Point { x, y: baseline_y },
            });
            x += f64::from(advance) / f64::from(font.units_per_em) * plan.font_size;
        }
    }
    output
}

fn pdf_number(value: f64) -> String {
    let mut output = format!("{value:.4}");
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
        if expected.degraded.is_some() && expected.frame.is_none() {
            continue;
        }
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
            validate_typeset_characters(
                expected.index,
                &rewrite.typeset_characters,
                &characters,
                context.config.baseline_tolerance_pt,
            )?;
        }
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
        if rewrite.typeset_characters.is_empty() {
            validate_output_raster(expected.index, input_raster, &raster)?;
        }
    }
    Ok(())
}

fn validate_typeset_characters(
    page_index: usize,
    expected: &[TypesetCharacter],
    actual: &[PageCharSnapshot],
    tolerance: f64,
) -> Result<()> {
    if expected.len() != actual.len() {
        return Err(output_mismatch(format!(
            "output page {} has {} typeset characters; expected {}",
            page_index + 1,
            actual.len(),
            expected.len()
        )));
    }
    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        if actual.unicode != Some(expected.unicode)
            || !point_close(expected.baseline_origin, actual.baseline_origin, tolerance)
        {
            return Err(output_mismatch(format!(
                "output page {} typeset character {index} differs from the plan",
                page_index + 1
            )));
        }
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
        self.extraction_equivalent
            + self.explained
            + self.strong_unicode_conflict
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
            UnicodeProvenance::ToUnicode | UnicodeProvenance::EmbeddedFontCmap => {
                counts.strong_unicode_conflict += 1;
            }
            UnicodeProvenance::SimpleEncoding => {
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
        }
    }

    fn config_with_test_output_fonts() -> crate::context::PipelineConfig {
        crate::context::PipelineConfig {
            output_fonts: Some(test_output_fonts()),
            ..crate::context::PipelineConfig::default()
        }
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

        let assignment = layout_assignment(&[model, fallback], bounds).unwrap();
        assert_eq!(assignment.label, LayoutLabel::Text);
        assert_eq!(assignment.source, LayoutSource::FallbackLine);

        fallback.label = LayoutLabel::Table;
        let assignment = layout_assignment(&[model, fallback], bounds).unwrap();
        assert_eq!(assignment.label, LayoutLabel::Table);
        assert_eq!(assignment.source, LayoutSource::Model);
        assert_eq!(assignment.reading_order, 3);
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
        translate(&mut document, &context).unwrap();
        typeset(&mut document, &context).unwrap();
        font_embed(&mut document, &context).unwrap();

        let rewrite = &document.rewrites[0];
        let font = &rewrite.embedded_fonts[0];
        assert_eq!(
            font.glyphs
                .iter()
                .map(|(_, value, _)| *value)
                .collect::<BTreeSet<_>>(),
            "MIMUS中文测试".chars().collect()
        );
        assert_eq!(font.base_font.len(), 6 + 1 + "NotoSansSC-Regular".len());
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
        let (bold, bold_cids) =
            build_embedded_font(&"MIMUS中文测试".chars().collect(), bold_source, true).unwrap();
        assert!(bold.base_font.ends_with("+NotoSansSC-Bold"));
        assert_eq!(bold_cids.len(), font.glyphs.len());
        assert_ne!(bold.font_bytes, font.font_bytes);
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
                    bold: output_font,
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
            } if missing_characters == "中文测试"
                && font_source == "test:missing-glyph"
                && font_sha256 == "6e1e40974dce5dca579f3f191dd7dcc9953e6e04165d69f36d01aa8242a24735"
        )));
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
        assert!(matches!(
            diagnostics.entries(),
            [Diagnostic::EngineCharacterAlignment {
                extraction_equivalent_count: 1,
                walk_only_count: 0,
                engine_only_count: 0,
                residual_count: 0,
                ..
            }]
        ));
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
            paragraph_preserved_reason(walked.iter().enumerate(), &BTreeSet::new()),
            Some(il::PreservedReason::UnreliableUnicode)
        );
        assert!(matches!(
            diagnostics.entries(),
            [Diagnostic::EngineCharacterAlignment {
                explained_count: 3,
                walk_only_count: 0,
                engine_only_count: 0,
                residual_count: 0,
                ..
            }]
        ));
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
        assert!(matches!(
            diagnostics.entries(),
            [Diagnostic::EngineCharacterAlignment {
                extraction_equivalent_count: 3,
                strong_unicode_conflict_count: 0,
                weak_unicode_conflict_count: 0,
                residual_count: 0,
                ..
            }]
        ));
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
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let mut walked = walk_page(&pdf, page_id).unwrap().characters;
        walked[0].unicode_provenance = UnicodeProvenance::SimpleEncoding;
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
                &alignment.weak_unicode_conflicts
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
    fn form_origin_characters_remain_passthrough_in_the_full_pipeline() {
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
            document.il.pages[0].paragraphs[0]
                .translated_text
                .as_deref(),
            Some("MIMUS")
        );
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert!(document.rewrites.is_empty());
        assert_eq!(
            std::fs::read(&output).unwrap(),
            std::fs::read(form_fixture()).unwrap()
        );
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
                left: 130.0,
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
                1,
                "output {output}"
            );
            assert!(
                document.diagnostics.entries().iter().any(|diagnostic| {
                    matches!(
                        diagnostic,
                        Diagnostic::PlaceholderViolation {
                            page_index: 0,
                            paragraph_index: 0,
                            violation,
                        } if *violation == expected
                    )
                }),
                "output {output}, expected {expected:?}, diagnostics {:?}",
                document.diagnostics.entries()
            );
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
            assert!(document.diagnostics.entries().iter().any(|diagnostic| {
                matches!(
                    diagnostic,
                    Diagnostic::DegradationSummary {
                        preserved_paragraphs,
                        ..
                    } if preserved_paragraphs.len() == 1
                        && preserved_paragraphs[0].placeholder_violation == Some(expected)
                )
            }));
        }
    }

    #[test]
    fn backend_echo_is_identity_output_without_degradation_or_warning() {
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
        assert_eq!(document.diagnostics.warning_count(), 0);
        assert!(matches!(
            document.diagnostics.entries(),
            [Diagnostic::TranslationIdentity {
                page_index: 0,
                paragraph_index: 0,
                request_characters: 5,
            }]
        ));
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

        assert_eq!(document.diagnostics.warning_count(), 1);
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
                        placeholder_violation: None,
                    }]
        )));
    }
}
