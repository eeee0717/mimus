use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use quick_xml::events::{BytesStart, Event};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const GEOMETRY_TOLERANCE_PT: f64 = 0.25;
const VECTOR_TOLERANCE_PT: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Rect {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InkViolationKind {
    MissingEvidence,
    UnexpectedEvidence,
    DuplicateEvidence,
    EmptyEvidence,
    InvalidBounds,
    OutsideCropBox,
    OutsideOwningContainer,
    MissingOutputGlyph,
    MissingOutputInk,
    OutputFontRouting,
    CrossParagraphCollision,
    RetainedInkCollision,
    InvalidFormulaOwnership,
}

impl InkViolationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::MissingEvidence => "missing_evidence",
            Self::UnexpectedEvidence => "unexpected_evidence",
            Self::DuplicateEvidence => "duplicate_evidence",
            Self::EmptyEvidence => "empty_evidence",
            Self::InvalidBounds => "invalid_bounds",
            Self::OutsideCropBox => "outside_crop_box",
            Self::OutsideOwningContainer => "outside_owning_container",
            Self::MissingOutputGlyph => "missing_output_glyph",
            Self::MissingOutputInk => "missing_output_ink",
            Self::OutputFontRouting => "output_font_routing",
            Self::CrossParagraphCollision => "cross_paragraph_collision",
            Self::RetainedInkCollision => "retained_ink_collision",
            Self::InvalidFormulaOwnership => "invalid_formula_ownership",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InkViolation {
    pub id: &'static str,
    pub kind: InkViolationKind,
    pub page_index: usize,
    pub reading_order: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_index: Option<usize>,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct InkAudit {
    pub status: &'static str,
    pub output_page_mode: &'static str,
    pub required_publications: usize,
    pub checked_publications: usize,
    pub checked_components: usize,
    pub violations: Vec<InkViolation>,
}

impl InkAudit {
    #[must_use]
    pub fn violation_count(&self) -> usize {
        self.violations.len()
    }

    #[must_use]
    pub fn counts_by_kind(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for violation in &self.violations {
            *counts
                .entry(violation.kind.as_str().to_owned())
                .or_insert(0) += 1;
        }
        counts
    }
}

#[derive(Debug, Deserialize)]
struct IlDocument {
    pages: Vec<IlPage>,
    #[serde(default)]
    publication_ink: Vec<PublicationInk>,
}

#[derive(Debug, Deserialize)]
struct IlPage {
    index: usize,
    paragraphs: Vec<IlParagraph>,
}

#[derive(Debug, Deserialize)]
struct IlParagraph {
    reading_order: usize,
    text: IlText,
    #[serde(default)]
    translated_text: Option<String>,
    #[serde(default)]
    preserved: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct IlText {
    chars: Vec<IlChar>,
}

#[derive(Debug, Deserialize)]
struct IlChar {
    unicode: Option<String>,
    #[serde(default)]
    implicit_space_before: bool,
}

#[derive(Debug, Deserialize)]
struct PublicationInk {
    page_index: usize,
    reading_order: usize,
    crop_box: Rect,
    admissible_container: Rect,
    components: Vec<PublicationInkComponent>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PublicationInkComponent {
    TranslatedText {
        ownership_group: usize,
        bounds: Rect,
        glyphs: Vec<PublicationGlyph>,
    },
    SourceTextReplay {
        ownership_group: usize,
        bounds: Rect,
        glyphs: Vec<PublicationGlyph>,
    },
    VectorPath {
        ownership_group: usize,
        bounds: Rect,
    },
    InlineImage {
        ownership_group: usize,
        bounds: Rect,
    },
}

impl PublicationInkComponent {
    fn bounds(&self) -> Rect {
        match self {
            Self::TranslatedText { bounds, .. }
            | Self::SourceTextReplay { bounds, .. }
            | Self::VectorPath { bounds, .. }
            | Self::InlineImage { bounds, .. } => *bounds,
        }
    }

    fn ownership_group(&self) -> usize {
        match self {
            Self::TranslatedText {
                ownership_group, ..
            }
            | Self::SourceTextReplay {
                ownership_group, ..
            }
            | Self::VectorPath {
                ownership_group, ..
            }
            | Self::InlineImage {
                ownership_group, ..
            } => *ownership_group,
        }
    }

    fn glyphs(&self) -> Option<&[PublicationGlyph]> {
        match self {
            Self::TranslatedText { glyphs, .. } | Self::SourceTextReplay { glyphs, .. } => {
                Some(glyphs)
            }
            Self::VectorPath { .. } | Self::InlineImage { .. } => None,
        }
    }

    fn trace_ink_kind(&self) -> Option<TraceInkKind> {
        match self {
            Self::VectorPath { .. } => Some(TraceInkKind::VectorPath),
            Self::InlineImage { .. } => Some(TraceInkKind::InlineImage),
            Self::TranslatedText { .. } | Self::SourceTextReplay { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PublicationGlyph {
    unicode: char,
    baseline_origin: Point,
    ink_bounds: Rect,
    #[serde(default)]
    font_slot: Option<OutputFontSlot>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OutputFontSlot {
    CjkRegular,
    CjkBold,
    LatinRegular,
    LatinBold,
    LatinSymbol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TraceInkKind {
    VectorPath,
    InlineImage,
}

#[derive(Clone, Debug)]
struct TraceInk {
    kind: TraceInkKind,
    bounds: Rect,
    segments: Vec<TraceSegment>,
    filled: bool,
    even_odd: bool,
    stroke_radius: f64,
    clips: Arc<Vec<TraceClip>>,
}

#[derive(Clone, Debug)]
struct TraceClip {
    bounds: Option<Rect>,
    segments: Vec<TraceSegment>,
    even_odd: bool,
}

#[derive(Clone, Copy, Debug)]
struct TraceSegment {
    start: Point,
    end: Point,
}

#[derive(Clone, Debug)]
struct TraceGlyph {
    unicode: char,
    origin: Point,
}

#[derive(Default)]
struct TracePage {
    left: f64,
    bottom: f64,
    top: f64,
    glyphs: Vec<TraceGlyph>,
    ink: Vec<TraceInk>,
}

#[derive(Clone, Copy)]
enum OutputPageMode {
    Default,
    Bilingual,
}

impl OutputPageMode {
    fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Bilingual => "bilingual",
        }
    }

    fn translated_page(self, page_index: usize) -> usize {
        match self {
            Self::Default => page_index,
            Self::Bilingual => 2 * page_index + 1,
        }
    }
}

pub fn audit_publication_ink_paths(il_path: &Path, output_pdf: &Path) -> Result<InkAudit> {
    let il = fs::read(il_path).with_context(|| format!("read {}", il_path.display()))?;
    let document: IlDocument = serde_json::from_slice(&il).context("parse public IL")?;
    if required_publications(&document).is_empty() && document.publication_ink.is_empty() {
        return Ok(InkAudit {
            status: "applicable",
            output_page_mode: "not_needed",
            required_publications: 0,
            checked_publications: 0,
            checked_components: 0,
            violations: Vec::new(),
        });
    }
    let trace = mutool_trace(output_pdf)?;
    audit_publication_ink(&il, &trace)
}

pub fn audit_publication_ink_evidence_path(il_path: &Path) -> Result<InkAudit> {
    let il = fs::read(il_path).with_context(|| format!("read {}", il_path.display()))?;
    let document: IlDocument = serde_json::from_slice(&il).context("parse public IL")?;
    let required = required_publications(&document);
    let mut violations = Vec::new();
    let mut evidence_by_owner = BTreeMap::<(usize, usize), Vec<usize>>::new();
    for (index, publication) in document.publication_ink.iter().enumerate() {
        evidence_by_owner
            .entry((publication.page_index, publication.reading_order))
            .or_default()
            .push(index);
    }
    for &(page_index, reading_order) in &required {
        match evidence_by_owner.get(&(page_index, reading_order)) {
            None => push_violation(
                &mut violations,
                InkViolationKind::MissingEvidence,
                page_index,
                reading_order,
                None,
                "non-identity published paragraph has no publication_ink entry",
            ),
            Some(indices) if indices.len() > 1 => push_violation(
                &mut violations,
                InkViolationKind::DuplicateEvidence,
                page_index,
                reading_order,
                None,
                "paragraph has more than one publication_ink entry",
            ),
            Some(_) => {}
        }
    }
    let mut checked_components = 0usize;
    for publication in &document.publication_ink {
        if !required.contains(&(publication.page_index, publication.reading_order)) {
            push_violation(
                &mut violations,
                InkViolationKind::UnexpectedEvidence,
                publication.page_index,
                publication.reading_order,
                None,
                "identity, preserved, or unknown paragraph exposes required publication evidence",
            );
        }
        if publication.components.is_empty() {
            push_violation(
                &mut violations,
                InkViolationKind::EmptyEvidence,
                publication.page_index,
                publication.reading_order,
                None,
                "publication evidence has no ink components",
            );
        }
        validate_formula_groups(publication, &mut violations);
        for (component_index, component) in publication.components.iter().enumerate() {
            checked_components += 1;
            let bounds = component.bounds();
            if !rect_is_valid(bounds) {
                push_violation(
                    &mut violations,
                    InkViolationKind::InvalidBounds,
                    publication.page_index,
                    publication.reading_order,
                    Some(component_index),
                    "component bounds are non-finite or empty",
                );
                continue;
            }
            if !rect_contains(publication.crop_box, bounds, GEOMETRY_TOLERANCE_PT) {
                push_violation(
                    &mut violations,
                    InkViolationKind::OutsideCropBox,
                    publication.page_index,
                    publication.reading_order,
                    Some(component_index),
                    "component extends outside the resolved CropBox",
                );
            }
            if !rect_contains(
                publication.admissible_container,
                bounds,
                GEOMETRY_TOLERANCE_PT,
            ) {
                push_violation(
                    &mut violations,
                    InkViolationKind::OutsideOwningContainer,
                    publication.page_index,
                    publication.reading_order,
                    Some(component_index),
                    "component extends outside its owning layout container",
                );
            }
            validate_glyph_ink_bounds(publication, component_index, component, &mut violations);
        }
    }
    check_component_collisions(&document.publication_ink, &mut violations);
    Ok(InkAudit {
        status: "exempt_final_artifact_observation",
        output_page_mode: "unavailable",
        required_publications: required.len(),
        checked_publications: document.publication_ink.len(),
        checked_components,
        violations,
    })
}

pub fn audit_publication_ink(il_json: &[u8], trace_xml: &str) -> Result<InkAudit> {
    let document: IlDocument = serde_json::from_slice(il_json).context("parse public IL")?;
    let trace_pages = parse_trace(trace_xml)?;
    let output_page_mode = if trace_pages.len() == document.pages.len() {
        OutputPageMode::Default
    } else if trace_pages.len() == document.pages.len() * 2 {
        OutputPageMode::Bilingual
    } else {
        bail!(
            "MuPDF trace has {} pages for {} IL pages",
            trace_pages.len(),
            document.pages.len()
        );
    };
    let required = required_publications(&document);
    let mut violations = Vec::new();
    let mut evidence_by_owner = BTreeMap::<(usize, usize), Vec<usize>>::new();
    for (index, publication) in document.publication_ink.iter().enumerate() {
        evidence_by_owner
            .entry((publication.page_index, publication.reading_order))
            .or_default()
            .push(index);
    }
    for &(page_index, reading_order) in &required {
        match evidence_by_owner.get(&(page_index, reading_order)) {
            None => push_violation(
                &mut violations,
                InkViolationKind::MissingEvidence,
                page_index,
                reading_order,
                None,
                "non-identity published paragraph has no publication_ink entry",
            ),
            Some(indices) if indices.len() > 1 => push_violation(
                &mut violations,
                InkViolationKind::DuplicateEvidence,
                page_index,
                reading_order,
                None,
                "paragraph has more than one publication_ink entry",
            ),
            Some(_) => {}
        }
    }
    for (&(page_index, reading_order), indices) in &evidence_by_owner {
        if !required.contains(&(page_index, reading_order)) {
            push_violation(
                &mut violations,
                InkViolationKind::UnexpectedEvidence,
                page_index,
                reading_order,
                None,
                "identity, preserved, or unknown paragraph exposes required publication evidence",
            );
        }
        if indices.len() > 1 && !required.contains(&(page_index, reading_order)) {
            push_violation(
                &mut violations,
                InkViolationKind::DuplicateEvidence,
                page_index,
                reading_order,
                None,
                "paragraph has more than one publication_ink entry",
            );
        }
    }

    let mut matched_glyphs = trace_pages
        .iter()
        .map(|page| vec![false; page.glyphs.len()])
        .collect::<Vec<_>>();
    let mut matched_ink = trace_pages
        .iter()
        .map(|page| vec![None; page.ink.len()])
        .collect::<Vec<Vec<Option<(usize, usize)>>>>();
    let mut checked_components = 0usize;
    for (publication_index, publication) in document.publication_ink.iter().enumerate() {
        let output_page_index = output_page_mode.translated_page(publication.page_index);
        let Some(trace_page) = trace_pages.get(output_page_index) else {
            continue;
        };
        let trace_offset = Point {
            x: publication.crop_box.left - trace_page.left,
            y: publication.crop_box.bottom - trace_page.bottom,
        };
        if publication.components.is_empty() {
            push_violation(
                &mut violations,
                InkViolationKind::EmptyEvidence,
                publication.page_index,
                publication.reading_order,
                None,
                "publication evidence has no ink components",
            );
        }
        validate_formula_groups(publication, &mut violations);
        for (component_index, component) in publication.components.iter().enumerate() {
            checked_components += 1;
            let bounds = component.bounds();
            if !rect_is_valid(bounds) {
                push_violation(
                    &mut violations,
                    InkViolationKind::InvalidBounds,
                    publication.page_index,
                    publication.reading_order,
                    Some(component_index),
                    "component bounds are non-finite or empty",
                );
                continue;
            }
            if !rect_contains(publication.crop_box, bounds, GEOMETRY_TOLERANCE_PT) {
                push_violation(
                    &mut violations,
                    InkViolationKind::OutsideCropBox,
                    publication.page_index,
                    publication.reading_order,
                    Some(component_index),
                    "component extends outside the resolved CropBox",
                );
            }
            if !rect_contains(
                publication.admissible_container,
                bounds,
                GEOMETRY_TOLERANCE_PT,
            ) {
                push_violation(
                    &mut violations,
                    InkViolationKind::OutsideOwningContainer,
                    publication.page_index,
                    publication.reading_order,
                    Some(component_index),
                    "component extends outside its owning layout container",
                );
            }
            validate_glyph_ink_bounds(publication, component_index, component, &mut violations);
            if let Some(glyphs) = component.glyphs() {
                match_component_glyphs(
                    publication,
                    component_index,
                    glyphs,
                    trace_page,
                    trace_offset,
                    &mut matched_glyphs[output_page_index],
                    &mut violations,
                );
            }
            if let Some(kind) = component.trace_ink_kind() {
                let tolerance = match kind {
                    TraceInkKind::VectorPath => VECTOR_TOLERANCE_PT,
                    TraceInkKind::InlineImage => GEOMETRY_TOLERANCE_PT,
                };
                let matching = trace_page.ink.iter().enumerate().find_map(|(index, ink)| {
                    let observed_bounds = effective_trace_ink_bounds(ink)?;
                    (matched_ink[output_page_index][index].is_none()
                        && ink.kind == kind
                        && rect_close(
                            translated_rect(observed_bounds, trace_offset.x, trace_offset.y),
                            bounds,
                            tolerance,
                        ))
                    .then_some(index)
                });
                if let Some(index) = matching {
                    matched_ink[output_page_index][index] =
                        Some((publication_index, component.ownership_group()));
                } else {
                    push_violation(
                        &mut violations,
                        InkViolationKind::MissingOutputInk,
                        publication.page_index,
                        publication.reading_order,
                        Some(component_index),
                        "declared vector or image component is absent at its final bounds",
                    );
                }
            }
        }
    }
    check_component_collisions(&document.publication_ink, &mut violations);
    check_trace_ink_collisions(
        &document.publication_ink,
        output_page_mode,
        &trace_pages,
        &matched_ink,
        &mut violations,
    );
    violations.sort_by_key(|violation| {
        (
            violation.page_index,
            violation.reading_order,
            violation.component_index,
            violation.kind as u8,
        )
    });
    Ok(InkAudit {
        status: "applicable",
        output_page_mode: output_page_mode.name(),
        required_publications: required.len(),
        checked_publications: document.publication_ink.len(),
        checked_components,
        violations,
    })
}

fn required_publications(document: &IlDocument) -> BTreeSet<(usize, usize)> {
    document
        .pages
        .iter()
        .flat_map(|page| {
            page.paragraphs.iter().filter_map(move |paragraph| {
                let source = source_text(paragraph);
                (paragraph.preserved.is_none()
                    && paragraph
                        .translated_text
                        .as_deref()
                        .is_some_and(|translated| translated != source))
                .then_some((page.index, paragraph.reading_order))
            })
        })
        .collect()
}

fn source_text(paragraph: &IlParagraph) -> String {
    let mut output = String::new();
    for character in &paragraph.text.chars {
        let Some(unicode) = character.unicode.as_deref() else {
            continue;
        };
        if character.implicit_space_before
            && !output.ends_with(char::is_whitespace)
            && !unicode.starts_with(char::is_whitespace)
        {
            output.push(' ');
        }
        output.push_str(unicode);
    }
    output
}

fn validate_formula_groups(publication: &PublicationInk, violations: &mut Vec<InkViolation>) {
    let mut groups = BTreeMap::<usize, (usize, usize, usize)>::new();
    for (component_index, component) in publication.components.iter().enumerate() {
        let group = component.ownership_group();
        match component {
            PublicationInkComponent::TranslatedText { .. } if group != 0 => push_violation(
                violations,
                InkViolationKind::InvalidFormulaOwnership,
                publication.page_index,
                publication.reading_order,
                Some(component_index),
                "translated output text must use ownership group 0",
            ),
            PublicationInkComponent::SourceTextReplay { glyphs, .. } => {
                let entry = groups.entry(group).or_default();
                entry.0 += 1;
                entry.2 += glyphs.len();
                if group == 0 {
                    push_violation(
                        violations,
                        InkViolationKind::InvalidFormulaOwnership,
                        publication.page_index,
                        publication.reading_order,
                        Some(component_index),
                        "source text replay must belong to a nonzero formula group",
                    );
                }
            }
            PublicationInkComponent::VectorPath { .. }
            | PublicationInkComponent::InlineImage { .. }
                if group > 0 =>
            {
                groups.entry(group).or_default().1 += 1;
            }
            _ => {}
        }
    }
    for (group, (source_text_components, _attached_ink, glyphs)) in groups {
        if source_text_components != 1 || glyphs == 0 {
            push_violation(
                violations,
                InkViolationKind::InvalidFormulaOwnership,
                publication.page_index,
                publication.reading_order,
                None,
                format!(
                    "formula ownership group {group} must have exactly one nonempty source replay"
                ),
            );
        }
    }
    for (component_index, component) in publication.components.iter().enumerate() {
        if component.ownership_group() > 0
            && matches!(
                component,
                PublicationInkComponent::VectorPath { .. }
                    | PublicationInkComponent::InlineImage { .. }
            )
            && !publication.components.iter().any(|candidate| {
                candidate.ownership_group() == component.ownership_group()
                    && matches!(candidate, PublicationInkComponent::SourceTextReplay { .. })
            })
        {
            push_violation(
                violations,
                InkViolationKind::InvalidFormulaOwnership,
                publication.page_index,
                publication.reading_order,
                Some(component_index),
                "formula vector or image has no source text owner",
            );
        }
    }
}

fn validate_glyph_ink_bounds(
    publication: &PublicationInk,
    component_index: usize,
    component: &PublicationInkComponent,
    violations: &mut Vec<InkViolation>,
) {
    let Some(glyphs) = component.glyphs() else {
        return;
    };
    if glyphs
        .iter()
        .any(|glyph| !rect_is_well_formed(glyph.ink_bounds))
    {
        push_violation(
            violations,
            InkViolationKind::InvalidBounds,
            publication.page_index,
            publication.reading_order,
            Some(component_index),
            "glyph ink bounds are non-finite or inverted",
        );
    } else if glyphs.iter().any(|glyph| {
        rect_is_valid(glyph.ink_bounds)
            && !rect_contains(component.bounds(), glyph.ink_bounds, GEOMETRY_TOLERANCE_PT)
    }) {
        push_violation(
            violations,
            InkViolationKind::InvalidBounds,
            publication.page_index,
            publication.reading_order,
            Some(component_index),
            "glyph ink extends outside its component summary bounds",
        );
    }
    validate_output_font_routing(publication, component_index, component, glyphs, violations);
}

fn validate_output_font_routing(
    publication: &PublicationInk,
    component_index: usize,
    component: &PublicationInkComponent,
    glyphs: &[PublicationGlyph],
    violations: &mut Vec<InkViolation>,
) {
    match component {
        PublicationInkComponent::TranslatedText { .. } => {
            // Historical IL predates slot provenance. Once one glyph in a
            // component carries it, require and validate the complete set.
            if !glyphs.iter().any(|glyph| glyph.font_slot.is_some()) {
                return;
            }
            for glyph in glyphs {
                let valid = matches!(
                    (
                        mimus_quality_contract::output_script_preference(glyph.unicode),
                        glyph.font_slot,
                    ),
                    (
                        mimus_quality_contract::OutputScriptPreference::Cjk,
                        Some(OutputFontSlot::CjkRegular | OutputFontSlot::CjkBold),
                    ) | (
                        mimus_quality_contract::OutputScriptPreference::Latin,
                        Some(
                            OutputFontSlot::LatinRegular
                                | OutputFontSlot::LatinBold
                                | OutputFontSlot::LatinSymbol,
                        ),
                    ) | (
                        mimus_quality_contract::OutputScriptPreference::Default,
                        Some(_)
                    )
                );
                if !valid {
                    push_violation(
                        violations,
                        InkViolationKind::OutputFontRouting,
                        publication.page_index,
                        publication.reading_order,
                        Some(component_index),
                        format!(
                            "translated glyph {:?} has incompatible output slot {:?}",
                            glyph.unicode, glyph.font_slot
                        ),
                    );
                }
            }
        }
        PublicationInkComponent::SourceTextReplay { .. } => {
            if glyphs.iter().any(|glyph| glyph.font_slot.is_some()) {
                push_violation(
                    violations,
                    InkViolationKind::OutputFontRouting,
                    publication.page_index,
                    publication.reading_order,
                    Some(component_index),
                    "source text replay must not claim an output-font slot",
                );
            }
        }
        PublicationInkComponent::VectorPath { .. }
        | PublicationInkComponent::InlineImage { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn match_component_glyphs(
    publication: &PublicationInk,
    component_index: usize,
    expected: &[PublicationGlyph],
    trace_page: &TracePage,
    trace_offset: Point,
    matched: &mut [bool],
    violations: &mut Vec<InkViolation>,
) {
    for glyph in expected {
        let matching = trace_page
            .glyphs
            .iter()
            .enumerate()
            .find_map(|(index, actual)| {
                (!matched[index]
                    && actual.unicode == glyph.unicode
                    && point_distance(
                        translated_point(actual.origin, trace_offset),
                        glyph.baseline_origin,
                    ) <= GEOMETRY_TOLERANCE_PT)
                    .then_some(index)
            });
        if let Some(index) = matching {
            matched[index] = true;
        } else {
            let nearest = trace_page
                .glyphs
                .iter()
                .filter(|actual| actual.unicode == glyph.unicode)
                .min_by(|left, right| {
                    point_distance(
                        translated_point(left.origin, trace_offset),
                        glyph.baseline_origin,
                    )
                    .total_cmp(&point_distance(
                        translated_point(right.origin, trace_offset),
                        glyph.baseline_origin,
                    ))
                });
            push_violation(
                violations,
                InkViolationKind::MissingOutputGlyph,
                publication.page_index,
                publication.reading_order,
                Some(component_index),
                nearest.map_or_else(
                    || {
                        format!(
                            "expected glyph {:?} is absent at ({:.3}, {:.3}); no matching Unicode was traced",
                            glyph.unicode, glyph.baseline_origin.x, glyph.baseline_origin.y
                        )
                    },
                    |actual| {
                        let actual = translated_point(actual.origin, trace_offset);
                        format!(
                            "expected glyph {:?} is absent at ({:.3}, {:.3}); nearest is ({:.3}, {:.3})",
                            glyph.unicode,
                            glyph.baseline_origin.x,
                            glyph.baseline_origin.y,
                            actual.x, actual.y
                        )
                    },
                ),
            );
        }
    }
}

fn check_component_collisions(publications: &[PublicationInk], violations: &mut Vec<InkViolation>) {
    for (left_index, left) in publications.iter().enumerate() {
        for right in &publications[left_index + 1..] {
            if left.page_index != right.page_index {
                continue;
            }
            for (component_index, left_component) in left.components.iter().enumerate() {
                let left_bounds = component_collision_bounds(left_component);
                if right.components.iter().any(|right_component| {
                    let right_bounds = component_collision_bounds(right_component);
                    left_bounds.iter().any(|&left_bound| {
                        right_bounds
                            .iter()
                            .any(|&right_bound| rects_overlap(left_bound, right_bound, 0.01))
                    })
                }) {
                    push_violation(
                        violations,
                        InkViolationKind::CrossParagraphCollision,
                        left.page_index,
                        left.reading_order,
                        Some(component_index),
                        format!(
                            "final ink intersects paragraph reading_order {}",
                            right.reading_order
                        ),
                    );
                }
            }
        }
    }
}

fn check_trace_ink_collisions(
    publications: &[PublicationInk],
    output_page_mode: OutputPageMode,
    trace_pages: &[TracePage],
    matched_ink: &[Vec<Option<(usize, usize)>>],
    violations: &mut Vec<InkViolation>,
) {
    for (publication_index, publication) in publications.iter().enumerate() {
        let output_page_index = output_page_mode.translated_page(publication.page_index);
        let Some(trace_page) = trace_pages.get(output_page_index) else {
            continue;
        };
        let trace_offset = Point {
            x: publication.crop_box.left - trace_page.left,
            y: publication.crop_box.bottom - trace_page.bottom,
        };
        for (component_index, component) in publication.components.iter().enumerate() {
            let Some(glyphs) = component.glyphs() else {
                continue;
            };
            let group = component.ownership_group();
            for (ink_index, ink) in trace_page.ink.iter().enumerate() {
                if matched_ink[output_page_index][ink_index] == Some((publication_index, group)) {
                    continue;
                }
                let Some(observed_bounds) = effective_trace_ink_bounds(ink) else {
                    continue;
                };
                let ink_bounds = translated_rect(observed_bounds, trace_offset.x, trace_offset.y);
                if ink.kind == TraceInkKind::InlineImage
                    && rect_contains(
                        ink_bounds,
                        publication.admissible_container,
                        GEOMETRY_TOLERANCE_PT,
                    )
                {
                    continue;
                }
                if (ink.filled || ink.kind == TraceInkKind::InlineImage)
                    && trace_fill_contains_rect(
                        ink,
                        translated_rect(
                            publication.admissible_container,
                            -trace_offset.x,
                            -trace_offset.y,
                        ),
                    )
                {
                    continue;
                }
                if let Some(glyph) = glyphs.iter().find(|glyph| {
                    rect_is_valid(glyph.ink_bounds)
                        && trace_ink_intersects_rect(
                            ink,
                            translated_rect(glyph.ink_bounds, -trace_offset.x, -trace_offset.y),
                        )
                }) {
                    push_violation(
                        violations,
                        InkViolationKind::RetainedInkCollision,
                        publication.page_index,
                        publication.reading_order,
                        Some(component_index),
                        format!(
                            "glyph {:?} ink [{:.3}, {:.3}, {:.3}, {:.3}] intersects retained {} [{:.3}, {:.3}, {:.3}, {:.3}]",
                            glyph.unicode,
                            glyph.ink_bounds.left,
                            glyph.ink_bounds.bottom,
                            glyph.ink_bounds.right,
                            glyph.ink_bounds.top,
                            match ink.kind {
                                TraceInkKind::VectorPath => "vector path",
                                TraceInkKind::InlineImage => "inline image",
                            },
                            ink_bounds.left,
                            ink_bounds.bottom,
                            ink_bounds.right,
                            ink_bounds.top,
                        ),
                    );
                }
            }
        }
    }
}

fn component_collision_bounds(component: &PublicationInkComponent) -> Vec<Rect> {
    match component {
        PublicationInkComponent::TranslatedText { glyphs, .. }
        | PublicationInkComponent::SourceTextReplay { glyphs, .. } => glyphs
            .iter()
            .map(|glyph| glyph.ink_bounds)
            .filter(|bounds| rect_is_valid(*bounds))
            .collect(),
        PublicationInkComponent::VectorPath { bounds, .. }
        | PublicationInkComponent::InlineImage { bounds, .. } => vec![*bounds],
    }
}

fn trace_ink_intersects_rect(ink: &TraceInk, rect: Rect) -> bool {
    if !effective_trace_ink_bounds(ink).is_some_and(|bounds| rects_overlap(bounds, rect, 0.01)) {
        return false;
    }
    if ink.clips.is_empty() {
        return match ink.kind {
            TraceInkKind::InlineImage => true,
            TraceInkKind::VectorPath if ink.filled => {
                ink.segments
                    .iter()
                    .any(|segment| segment_intersects_rect(*segment, rect))
                    || rect_corners(rect)
                        .into_iter()
                        .any(|point| point_in_filled_segments(point, &ink.segments, ink.even_odd))
            }
            TraceInkKind::VectorPath => {
                let expanded = expand_rect(rect, ink.stroke_radius);
                ink.segments
                    .iter()
                    .any(|segment| segment_intersects_rect(*segment, expanded))
            }
        };
    }
    common_ink_clip_candidates(ink, rect)
        .into_iter()
        .any(|point| point_in_rect(point, rect) && trace_ink_contains_point(ink, point))
}

fn trace_fill_contains_rect(ink: &TraceInk, rect: Rect) -> bool {
    let points = rect_corners(rect).into_iter().chain([Point {
        x: (rect.left + rect.right) / 2.0,
        y: (rect.bottom + rect.top) / 2.0,
    }]);
    points
        .into_iter()
        .all(|point| trace_ink_contains_point(ink, point))
        && !ink
            .segments
            .iter()
            .any(|segment| segment_intersects_rect(*segment, rect))
        && !ink.clips.iter().any(|clip| {
            clip.segments
                .iter()
                .any(|segment| segment_intersects_rect(*segment, rect))
        })
}

fn effective_trace_ink_bounds(ink: &TraceInk) -> Option<Rect> {
    ink.clips.iter().try_fold(ink.bounds, |bounds, clip| {
        intersect_rect(bounds, clip.bounds?)
    })
}

fn trace_ink_contains_point(ink: &TraceInk, point: Point) -> bool {
    let inside_ink = match ink.kind {
        TraceInkKind::InlineImage => point_in_rect(point, ink.bounds),
        TraceInkKind::VectorPath if ink.filled => {
            point_in_filled_segments(point, &ink.segments, ink.even_odd)
        }
        TraceInkKind::VectorPath => ink
            .segments
            .iter()
            .any(|segment| point_segment_distance(point, *segment) <= ink.stroke_radius.max(0.01)),
    };
    inside_ink
        && ink.clips.iter().all(|clip| {
            clip.bounds
                .is_some_and(|bounds| point_in_rect(point, bounds))
                && point_in_filled_segments(point, &clip.segments, clip.even_odd)
        })
}

fn common_ink_clip_candidates(ink: &TraceInk, rect: Rect) -> Vec<Point> {
    let query_segments = rect_segments(rect).to_vec();
    let ink_segments = match ink.kind {
        TraceInkKind::InlineImage => rect_segments(ink.bounds).to_vec(),
        TraceInkKind::VectorPath => ink.segments.clone(),
    };
    let mut boundaries = Vec::<&[TraceSegment]>::with_capacity(ink.clips.len() + 2);
    boundaries.push(&ink_segments);
    for clip in ink.clips.iter() {
        boundaries.push(&clip.segments);
    }
    boundaries.push(&query_segments);

    let mut candidates = rect_corners(rect).to_vec();
    candidates.push(Point {
        x: (rect.left + rect.right) / 2.0,
        y: (rect.bottom + rect.top) / 2.0,
    });
    for segments in &boundaries {
        for segment in *segments {
            candidates.extend([
                segment.start,
                segment.end,
                Point {
                    x: (segment.start.x + segment.end.x) / 2.0,
                    y: (segment.start.y + segment.end.y) / 2.0,
                },
            ]);
        }
    }
    for left_index in 0..boundaries.len() {
        for right in &boundaries[left_index + 1..] {
            for &left_segment in boundaries[left_index] {
                for &right_segment in *right {
                    if let Some(point) = segment_intersection(left_segment, right_segment) {
                        candidates.push(point);
                    }
                }
            }
        }
    }
    if !ink.filled && ink.kind == TraceInkKind::VectorPath {
        for segments in &boundaries[1..] {
            for boundary in *segments {
                for &path_segment in &ink.segments {
                    candidates.push(project_point_to_segment(boundary.start, path_segment));
                    candidates.push(project_point_to_segment(boundary.end, path_segment));
                }
            }
        }
    }
    candidates
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

fn point_in_filled_segments(point: Point, segments: &[TraceSegment], even_odd: bool) -> bool {
    if segments
        .iter()
        .any(|segment| point_segment_distance(point, *segment) <= 1e-7)
    {
        return true;
    }
    if even_odd {
        return segments.iter().fold(false, |inside, segment| {
            let crosses = (segment.start.y > point.y) != (segment.end.y > point.y)
                && point.x
                    < (segment.end.x - segment.start.x) * (point.y - segment.start.y)
                        / (segment.end.y - segment.start.y)
                        + segment.start.x;
            inside ^ crosses
        });
    }
    let winding = segments.iter().fold(0_i32, |winding, segment| {
        let cross = (segment.end.x - segment.start.x) * (point.y - segment.start.y)
            - (point.x - segment.start.x) * (segment.end.y - segment.start.y);
        if segment.start.y <= point.y && segment.end.y > point.y && cross > 0.0 {
            winding + 1
        } else if segment.start.y > point.y && segment.end.y <= point.y && cross < 0.0 {
            winding - 1
        } else {
            winding
        }
    });
    winding != 0
}

fn point_in_rect(point: Point, rect: Rect) -> bool {
    point.x >= rect.left - 1e-7
        && point.x <= rect.right + 1e-7
        && point.y >= rect.bottom - 1e-7
        && point.y <= rect.top + 1e-7
}

fn rect_segments(rect: Rect) -> [TraceSegment; 4] {
    let [bottom_left, top_left, bottom_right, top_right] = rect_corners(rect);
    [
        TraceSegment {
            start: bottom_left,
            end: bottom_right,
        },
        TraceSegment {
            start: bottom_right,
            end: top_right,
        },
        TraceSegment {
            start: top_right,
            end: top_left,
        },
        TraceSegment {
            start: top_left,
            end: bottom_left,
        },
    ]
}

fn point_segment_distance(point: Point, segment: TraceSegment) -> f64 {
    let projected = project_point_to_segment(point, segment);
    point_distance(point, projected)
}

fn project_point_to_segment(point: Point, segment: TraceSegment) -> Point {
    let delta_x = segment.end.x - segment.start.x;
    let delta_y = segment.end.y - segment.start.y;
    let length_squared = delta_x * delta_x + delta_y * delta_y;
    if length_squared <= 1e-18 {
        return segment.start;
    }
    let ratio = ((point.x - segment.start.x) * delta_x + (point.y - segment.start.y) * delta_y)
        / length_squared;
    let ratio = ratio.clamp(0.0, 1.0);
    Point {
        x: segment.start.x + ratio * delta_x,
        y: segment.start.y + ratio * delta_y,
    }
}

fn segment_intersection(left: TraceSegment, right: TraceSegment) -> Option<Point> {
    let left_delta = Point {
        x: left.end.x - left.start.x,
        y: left.end.y - left.start.y,
    };
    let right_delta = Point {
        x: right.end.x - right.start.x,
        y: right.end.y - right.start.y,
    };
    let denominator = left_delta.x * right_delta.y - left_delta.y * right_delta.x;
    if denominator.abs() <= 1e-12 {
        return None;
    }
    let origin_delta = Point {
        x: right.start.x - left.start.x,
        y: right.start.y - left.start.y,
    };
    let left_ratio =
        (origin_delta.x * right_delta.y - origin_delta.y * right_delta.x) / denominator;
    let right_ratio = (origin_delta.x * left_delta.y - origin_delta.y * left_delta.x) / denominator;
    if !(-1e-9..=1.0 + 1e-9).contains(&left_ratio) || !(-1e-9..=1.0 + 1e-9).contains(&right_ratio) {
        return None;
    }
    Some(Point {
        x: left.start.x + left_ratio * left_delta.x,
        y: left.start.y + left_ratio * left_delta.y,
    })
}

fn intersect_rect(left: Rect, right: Rect) -> Option<Rect> {
    let intersection = Rect {
        left: left.left.max(right.left),
        bottom: left.bottom.max(right.bottom),
        right: left.right.min(right.right),
        top: left.top.min(right.top),
    };
    (intersection.right > intersection.left && intersection.top > intersection.bottom)
        .then_some(intersection)
}

fn segment_intersects_rect(segment: TraceSegment, rect: Rect) -> bool {
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

fn push_violation(
    violations: &mut Vec<InkViolation>,
    kind: InkViolationKind,
    page_index: usize,
    reading_order: usize,
    component_index: Option<usize>,
    detail: impl Into<String>,
) {
    violations.push(InkViolation {
        id: "INK-01",
        kind,
        page_index,
        reading_order,
        component_index,
        detail: detail.into(),
    });
}

fn mutool_trace(path: &Path) -> Result<String> {
    let temp = tempfile::NamedTempFile::new()?;
    let output = Command::new("mutool")
        .args(["draw", "-q", "-F", "trace", "-o"])
        .arg(temp.path())
        .arg(path)
        .output()
        .with_context(|| format!("run mutool trace for {}", path.display()))?;
    if !output.status.success() {
        bail!(
            "mutool trace failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::read_to_string(temp.path()).context("read MuPDF trace output")
}

fn parse_trace(xml: &str) -> Result<Vec<TracePage>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut pages = Vec::<TracePage>::new();
    let mut current_page = None::<TracePage>;
    let mut text_transform = None::<[f64; 6]>;
    let mut path = None::<TracePath>;
    let mut clips = Arc::new(Vec::<TraceClip>::new());
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(tag) if tag.name().as_ref() == b"page" => {
                let media = trace_numbers(&tag, b"mediabox")?;
                Arc::make_mut(&mut clips).clear();
                current_page = Some(TracePage {
                    left: media.first().copied().unwrap_or(0.0),
                    bottom: media.get(1).copied().unwrap_or(0.0),
                    top: media.get(3).copied().unwrap_or(0.0),
                    ..TracePage::default()
                });
            }
            Event::End(tag) if tag.name().as_ref() == b"page" => {
                pages.push(
                    current_page
                        .take()
                        .context("trace page end without page start")?,
                );
            }
            Event::Start(tag) if tag.name().as_ref() == b"fill_text" => {
                text_transform = Some(trace_matrix(&tag)?);
            }
            Event::End(tag) if tag.name().as_ref() == b"fill_text" => text_transform = None,
            Event::Start(tag)
                if matches!(
                    tag.name().as_ref(),
                    b"stroke_path" | b"fill_path" | b"clip_path"
                ) =>
            {
                let filled = tag.name().as_ref() != b"stroke_path";
                let linewidth = if filled {
                    0.0
                } else {
                    trace_f64(&tag, b"linewidth")?
                };
                path = Some(TracePath {
                    tag: tag.name().as_ref().to_vec(),
                    transform: trace_matrix(&tag)?,
                    linewidth,
                    visible: trace_optional_f64(&tag, b"alpha")?.unwrap_or(1.0) > 0.0,
                    filled,
                    even_odd: trace_optional_attr(&tag, b"winding")?.as_deref() == Some("eofill"),
                    segments: Vec::new(),
                    current: None,
                    subpath_start: None,
                });
            }
            Event::End(tag)
                if matches!(
                    tag.name().as_ref(),
                    b"stroke_path" | b"fill_path" | b"clip_path"
                ) =>
            {
                if let (Some(page), Some(mut path)) = (current_page.as_mut(), path.take()) {
                    path.finish();
                    if path.tag != tag.name().as_ref() {
                        bail!("MuPDF trace path tags are not properly nested");
                    }
                    if path.tag == b"clip_path" {
                        Arc::make_mut(&mut clips).push(TraceClip {
                            bounds: segment_bounds(&path.segments),
                            segments: path.segments,
                            even_odd: path.even_odd,
                        });
                    } else if path.visible
                        && let Some(bounds) = segment_bounds(&path.segments)
                    {
                        let stroke_radius = path.linewidth * matrix_max_scale(path.transform) / 2.0;
                        page.ink.push(TraceInk {
                            kind: TraceInkKind::VectorPath,
                            bounds: expand_rect(bounds, stroke_radius),
                            segments: path.segments,
                            filled: path.filled,
                            even_odd: path.even_odd,
                            stroke_radius,
                            clips: Arc::clone(&clips),
                        });
                    }
                }
            }
            Event::Empty(tag) if tag.name().as_ref() == b"g" => {
                if let (Some(page), Some(transform), Some(unicode)) = (
                    current_page.as_mut(),
                    text_transform,
                    trace_attr(&tag, b"unicode")?.chars().next(),
                ) {
                    let device =
                        matrix_point(transform, trace_f64(&tag, b"x")?, trace_f64(&tag, b"y")?);
                    page.glyphs.push(TraceGlyph {
                        unicode,
                        origin: Point {
                            x: page.left + device.x,
                            y: page.top - device.y,
                        },
                    });
                }
            }
            Event::Empty(tag)
                if matches!(
                    tag.name().as_ref(),
                    b"moveto" | b"lineto" | b"curveto" | b"quadto"
                ) =>
            {
                if let Some(path) = path.as_mut() {
                    let page = current_page
                        .as_ref()
                        .context("trace path point outside page")?;
                    let point = |x_key, y_key| -> Result<Point> {
                        let device = matrix_point(
                            path.transform,
                            trace_f64(&tag, x_key)?,
                            trace_f64(&tag, y_key)?,
                        );
                        Ok(Point {
                            x: page.left + device.x,
                            y: page.top - device.y,
                        })
                    };
                    match tag.name().as_ref() {
                        b"moveto" => path.move_to(point(b"x", b"y")?),
                        b"lineto" => path.line_to(point(b"x", b"y")?),
                        b"curveto" => path.curve_to(
                            point(b"x1", b"y1")?,
                            point(b"x2", b"y2")?,
                            point(b"x3", b"y3")?,
                        ),
                        b"quadto" => path.quad_to(point(b"x1", b"y1")?, point(b"x2", b"y2")?),
                        _ => unreachable!(),
                    }
                }
            }
            Event::Empty(tag) if tag.name().as_ref() == b"closepath" => {
                if let Some(path) = path.as_mut() {
                    path.close_subpath();
                }
            }
            Event::Empty(tag) if tag.name().as_ref() == b"fill_image" => {
                if let Some(page) = current_page.as_mut() {
                    let corners = trace_unit_rect(page, trace_matrix(&tag)?);
                    if let Some(bounds) = points_bounds(&corners) {
                        page.ink.push(TraceInk {
                            kind: TraceInkKind::InlineImage,
                            bounds,
                            segments: Vec::new(),
                            filled: false,
                            even_odd: false,
                            stroke_radius: 0.0,
                            clips: Arc::clone(&clips),
                        });
                    }
                }
            }
            Event::Empty(tag) if tag.name().as_ref() == b"clip_image_mask" => {
                let page = current_page
                    .as_ref()
                    .context("trace image-mask clip outside page")?;
                let corners = trace_unit_rect(page, trace_matrix(&tag)?);
                let bounds = points_bounds(&corners);
                Arc::make_mut(&mut clips).push(TraceClip {
                    bounds,
                    segments: bounds.map_or_else(Vec::new, |bounds| rect_segments(bounds).to_vec()),
                    even_odd: false,
                });
            }
            Event::Empty(tag) if tag.name().as_ref() == b"pop_clip" => {
                Arc::make_mut(&mut clips)
                    .pop()
                    .context("MuPDF trace clip stack underflow")?;
            }
            _ => {}
        }
    }
    Ok(pages)
}

fn trace_unit_rect(page: &TracePage, transform: [f64; 6]) -> [Point; 4] {
    [
        matrix_point(transform, 0.0, 0.0),
        matrix_point(transform, 1.0, 0.0),
        matrix_point(transform, 0.0, 1.0),
        matrix_point(transform, 1.0, 1.0),
    ]
    .map(|point| Point {
        x: page.left + point.x,
        y: page.top - point.y,
    })
}

struct TracePath {
    tag: Vec<u8>,
    transform: [f64; 6],
    linewidth: f64,
    visible: bool,
    filled: bool,
    even_odd: bool,
    segments: Vec<TraceSegment>,
    current: Option<Point>,
    subpath_start: Option<Point>,
}

impl TracePath {
    fn move_to(&mut self, point: Point) {
        if self.filled {
            self.close_subpath();
        }
        self.current = Some(point);
        self.subpath_start = Some(point);
    }

    fn line_to(&mut self, point: Point) {
        if let Some(start) = self.current {
            self.segments.push(TraceSegment { start, end: point });
        }
        self.current = Some(point);
        self.subpath_start.get_or_insert(point);
    }

    fn curve_to(&mut self, control1: Point, control2: Point, end: Point) {
        let Some(start) = self.current else {
            self.move_to(end);
            return;
        };
        let mut previous = start;
        for step in 1..=16 {
            let t = f64::from(step) / 16.0;
            let inverse = 1.0 - t;
            let point = Point {
                x: inverse.powi(3) * start.x
                    + 3.0 * inverse.powi(2) * t * control1.x
                    + 3.0 * inverse * t * t * control2.x
                    + t.powi(3) * end.x,
                y: inverse.powi(3) * start.y
                    + 3.0 * inverse.powi(2) * t * control1.y
                    + 3.0 * inverse * t * t * control2.y
                    + t.powi(3) * end.y,
            };
            self.segments.push(TraceSegment {
                start: previous,
                end: point,
            });
            previous = point;
        }
        self.current = Some(end);
    }

    fn quad_to(&mut self, control: Point, end: Point) {
        let Some(start) = self.current else {
            self.move_to(end);
            return;
        };
        let mut previous = start;
        for step in 1..=16 {
            let t = f64::from(step) / 16.0;
            let inverse = 1.0 - t;
            let point = Point {
                x: inverse * inverse * start.x + 2.0 * inverse * t * control.x + t * t * end.x,
                y: inverse * inverse * start.y + 2.0 * inverse * t * control.y + t * t * end.y,
            };
            self.segments.push(TraceSegment {
                start: previous,
                end: point,
            });
            previous = point;
        }
        self.current = Some(end);
    }

    fn close_subpath(&mut self) {
        if let (Some(start), Some(end)) = (self.subpath_start, self.current)
            && (start.x - end.x).abs() + (start.y - end.y).abs() > 1e-9
        {
            self.segments.push(TraceSegment {
                start: end,
                end: start,
            });
        }
        self.current = self.subpath_start;
        self.subpath_start = None;
    }

    fn finish(&mut self) {
        if self.filled {
            self.close_subpath();
        }
    }
}

fn segment_bounds(segments: &[TraceSegment]) -> Option<Rect> {
    segments
        .iter()
        .flat_map(|segment| [segment.start, segment.end])
        .map(|point| Rect {
            left: point.x,
            bottom: point.y,
            right: point.x,
            top: point.y,
        })
        .reduce(|left, right| Rect {
            left: left.left.min(right.left),
            bottom: left.bottom.min(right.bottom),
            right: left.right.max(right.right),
            top: left.top.max(right.top),
        })
}

fn trace_attr(tag: &BytesStart<'_>, key: &[u8]) -> Result<String> {
    trace_optional_attr(tag, key)?.context("trace attribute missing")
}

fn trace_optional_attr(tag: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>> {
    let Some(attribute) = tag.attributes().find(|attribute| {
        attribute
            .as_ref()
            .is_ok_and(|value| value.key.as_ref() == key)
    }) else {
        return Ok(None);
    };
    let attribute = attribute?;
    Ok(Some(
        attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
            .into_owned(),
    ))
}

fn trace_optional_f64(tag: &BytesStart<'_>, key: &[u8]) -> Result<Option<f64>> {
    trace_optional_attr(tag, key)?
        .map(|value| value.parse().map_err(Into::into))
        .transpose()
}

fn matrix_max_scale([a, b, c, d, _, _]: [f64; 6]) -> f64 {
    let squared_sum = a * a + b * b + c * c + d * d;
    let determinant = a * d - b * c;
    let discriminant = (squared_sum * squared_sum - 4.0 * determinant * determinant).max(0.0);
    ((squared_sum + discriminant.sqrt()) / 2.0).sqrt()
}

fn trace_f64(tag: &BytesStart<'_>, key: &[u8]) -> Result<f64> {
    Ok(trace_attr(tag, key)?.parse()?)
}

fn trace_numbers(tag: &BytesStart<'_>, key: &[u8]) -> Result<Vec<f64>> {
    trace_attr(tag, key)?
        .split_whitespace()
        .map(|value| Ok(value.parse()?))
        .collect()
}

fn trace_matrix(tag: &BytesStart<'_>) -> Result<[f64; 6]> {
    trace_numbers(tag, b"transform")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("trace transform is not a six-number matrix"))
}

fn matrix_point(matrix: [f64; 6], x: f64, y: f64) -> Point {
    Point {
        x: matrix[0] * x + matrix[2] * y + matrix[4],
        y: matrix[1] * x + matrix[3] * y + matrix[5],
    }
}

fn points_bounds(points: &[Point]) -> Option<Rect> {
    Some(Rect {
        left: points.iter().map(|point| point.x).reduce(f64::min)?,
        bottom: points.iter().map(|point| point.y).reduce(f64::min)?,
        right: points.iter().map(|point| point.x).reduce(f64::max)?,
        top: points.iter().map(|point| point.y).reduce(f64::max)?,
    })
}

fn expand_rect(rect: Rect, amount: f64) -> Rect {
    Rect {
        left: rect.left - amount,
        bottom: rect.bottom - amount,
        right: rect.right + amount,
        top: rect.top + amount,
    }
}

fn translated_rect(rect: Rect, delta_x: f64, delta_y: f64) -> Rect {
    Rect {
        left: rect.left + delta_x,
        bottom: rect.bottom + delta_y,
        right: rect.right + delta_x,
        top: rect.top + delta_y,
    }
}

fn translated_point(point: Point, delta: Point) -> Point {
    Point {
        x: point.x + delta.x,
        y: point.y + delta.y,
    }
}

fn rect_is_valid(rect: Rect) -> bool {
    rect_is_well_formed(rect) && rect.right > rect.left && rect.top > rect.bottom
}

fn rect_is_well_formed(rect: Rect) -> bool {
    [rect.left, rect.bottom, rect.right, rect.top]
        .into_iter()
        .all(f64::is_finite)
        && rect.right >= rect.left
        && rect.top >= rect.bottom
}

fn rect_contains(outer: Rect, inner: Rect, tolerance: f64) -> bool {
    inner.left >= outer.left - tolerance
        && inner.bottom >= outer.bottom - tolerance
        && inner.right <= outer.right + tolerance
        && inner.top <= outer.top + tolerance
}

fn rects_overlap(left: Rect, right: Rect, tolerance: f64) -> bool {
    left.right > right.left + tolerance
        && right.right > left.left + tolerance
        && left.top > right.bottom + tolerance
        && right.top > left.bottom + tolerance
}

fn rect_close(left: Rect, right: Rect, tolerance: f64) -> bool {
    (left.left - right.left).abs() <= tolerance
        && (left.bottom - right.bottom).abs() <= tolerance
        && (left.right - right.right).abs() <= tolerance
        && (left.top - right.top).abs() <= tolerance
}

fn point_distance(left: Point, right: Point) -> f64 {
    ((left.x - right.x).powi(2) + (left.y - right.y).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn il(components: Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "pages": [{
                "index": 0,
                "geometry": {"width": 100.0, "height": 100.0, "rotate_degrees": 0},
                "paragraphs": [{
                    "reading_order": 0,
                    "bounds": {"left": 20.0, "bottom": 18.0, "right": 30.0, "top": 25.0},
                    "text": {"kind": "chars", "chars": [{
                        "unicode": "a", "implicit_space_before": false
                    }]},
                    "translated_text": "中"
                }]
            }],
            "publication_ink": [{
                "page_index": 0,
                "reading_order": 0,
                "crop_box": {"left": 0.0, "bottom": 0.0, "right": 100.0, "top": 100.0},
                "admissible_container": {
                    "left": 10.0, "bottom": 10.0, "right": 90.0, "top": 90.0
                },
                "components": components
            }]
        }))
        .unwrap()
    }

    fn text_component(bounds: Value) -> Value {
        text_component_with_glyph_bounds(
            bounds,
            serde_json::json!({
                "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
            }),
        )
    }

    fn text_component_with_glyph_bounds(bounds: Value, ink_bounds: Value) -> Value {
        serde_json::json!({
            "kind": "translated_text",
            "ownership_group": 0,
            "bounds": bounds,
            "glyphs": [{
                "unicode": "中",
                "baseline_origin": {"x": 20.0, "y": 20.0},
                "ink_bounds": ink_bounds
            }]
        })
    }

    fn trace(extra: &str) -> String {
        format!(
            r#"<document><page number="1" mediabox="0 0 100 100">
            <fill_text transform="1 0 0 1 0 0"><g unicode="中" x="20" y="80"/></fill_text>
            {extra}</page></document>"#
        )
    }

    fn kinds(audit: &InkAudit) -> BTreeSet<InkViolationKind> {
        audit.violations.iter().map(|value| value.kind).collect()
    }

    #[test]
    fn valid_final_ink_is_accepted() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let audit = audit_publication_ink(&input, &trace("")).unwrap();
        assert_eq!(audit.violation_count(), 0);
        assert_eq!(
            (audit.required_publications, audit.checked_components),
            (1, 1)
        );
    }

    #[test]
    fn instrumented_output_font_slots_use_the_shared_script_policy() {
        let component = |unicode, font_slot| {
            serde_json::json!({
                "kind": "translated_text",
                "ownership_group": 0,
                "bounds": {"left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0},
                "glyphs": [{
                    "unicode": unicode,
                    "baseline_origin": {"x": 20.0, "y": 20.0},
                    "ink_bounds": {
                        "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
                    },
                    "font_slot": font_slot
                }]
            })
        };
        let valid = audit_publication_ink(
            &il(serde_json::json!([component("中", "cjk_regular")])),
            &trace(""),
        )
        .unwrap();
        assert!(!kinds(&valid).contains(&InkViolationKind::OutputFontRouting));

        let invalid = audit_publication_ink(
            &il(serde_json::json!([component("中", "latin_regular")])),
            &trace(""),
        )
        .unwrap();
        assert!(kinds(&invalid).contains(&InkViolationKind::OutputFontRouting));
    }

    #[test]
    fn translated_text_outside_its_container_is_rejected() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 5.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let audit = audit_publication_ink(&input, &trace("")).unwrap();
        assert!(kinds(&audit).contains(&InkViolationKind::OutsideOwningContainer));
    }

    #[test]
    fn translated_text_over_a_retained_path_is_rejected() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<stroke_path linewidth="1" transform="1 0 0 1 0 0">
            <moveto x="20" y="80"/><lineto x="30" y="80"/></stroke_path>"#,
        );
        let audit = audit_publication_ink(&input, &trace).unwrap();
        assert!(kinds(&audit).contains(&InkViolationKind::RetainedInkCollision));
    }

    #[test]
    fn retained_path_outside_its_active_clip_is_accepted() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<clip_path winding="nonzero" transform="1 0 0 1 0 0">
            <moveto x="0" y="0"/><lineto x="100" y="0"/><lineto x="100" y="50"/>
            <lineto x="0" y="50"/><closepath/></clip_path>
            <stroke_path linewidth="1" transform="1 0 0 1 0 0">
            <moveto x="20" y="80"/><lineto x="30" y="80"/></stroke_path><pop_clip/>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn clip_pop_restores_retained_path_collision() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<clip_path winding="nonzero" transform="1 0 0 1 0 0">
            <moveto x="0" y="0"/><lineto x="100" y="0"/><lineto x="100" y="50"/>
            <lineto x="0" y="50"/><closepath/></clip_path><pop_clip/>
            <stroke_path linewidth="1" transform="1 0 0 1 0 0">
            <moveto x="20" y="80"/><lineto x="30" y="80"/></stroke_path>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert!(kinds(&audit).contains(&InkViolationKind::RetainedInkCollision));
    }

    #[test]
    fn even_odd_clip_hole_excludes_retained_fill() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<clip_path winding="eofill" transform="1 0 0 1 0 0">
            <moveto x="10" y="70"/><lineto x="40" y="70"/><lineto x="40" y="90"/>
            <lineto x="10" y="90"/><closepath/>
            <moveto x="18" y="74"/><lineto x="31" y="74"/><lineto x="31" y="83"/>
            <lineto x="18" y="83"/><closepath/></clip_path>
            <fill_path winding="nonzero" transform="1 0 0 1 0 0">
            <moveto x="20" y="82"/><lineto x="30" y="82"/><lineto x="30" y="75"/>
            <lineto x="20" y="75"/><closepath/></fill_path><pop_clip/>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn retained_image_outside_its_active_clip_is_accepted() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<clip_path winding="nonzero" transform="1 0 0 1 0 0">
            <moveto x="0" y="0"/><lineto x="100" y="0"/><lineto x="100" y="50"/>
            <lineto x="0" y="50"/><closepath/></clip_path>
            <fill_image transform="10 0 0 -10 20 80"/><pop_clip/>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn retained_fill_hull_without_polygon_overlap_is_accepted() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<fill_path winding="nonzero" transform="1 0 0 1 0 0">
            <moveto x="10" y="90"/><lineto x="20" y="90"/><lineto x="10" y="80"/>
            <closepath/><moveto x="30" y="70"/><lineto x="40" y="70"/>
            <lineto x="40" y="60"/><closepath/></fill_path>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn retained_partial_fill_intersecting_glyph_is_rejected() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<fill_path winding="nonzero" transform="1 0 0 1 0 0">
            <moveto x="20" y="82"/><lineto x="25" y="82"/><lineto x="25" y="75"/>
            <lineto x="20" y="75"/><closepath/></fill_path>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert!(kinds(&audit).contains(&InkViolationKind::RetainedInkCollision));
    }

    #[test]
    fn full_container_fill_is_accepted_as_background() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<fill_path winding="nonzero" transform="1 0 0 1 0 0">
            <moveto x="0" y="100"/><lineto x="100" y="100"/>
            <lineto x="100" y="0"/><lineto x="0" y="0"/><closepath/></fill_path>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn retained_path_in_line_whitespace_is_accepted() {
        let input = il(serde_json::json!([text_component_with_glyph_bounds(
            serde_json::json!({
                "left": 10.0, "bottom": 18.0, "right": 90.0, "top": 25.0
            }),
            serde_json::json!({
                "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
            })
        )]));
        let trace = trace(
            r#"<stroke_path linewidth="1" transform="1 0 0 1 0 0">
            <moveto x="50" y="80"/><lineto x="60" y="80"/></stroke_path>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn invisible_retained_path_is_not_ink() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<stroke_path linewidth="1" alpha="0" transform="1 0 0 1 0 0">
            <moveto x="20" y="80"/><lineto x="30" y="80"/></stroke_path>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn retained_stroke_width_is_scaled_into_page_space() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = trace(
            r#"<stroke_path linewidth="100" transform=".01 0 0 .01 20 60">
            <moveto x="0" y="0"/><lineto x="1000" y="0"/></stroke_path>"#,
        );

        let audit = audit_publication_ink(&input, &trace).unwrap();

        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn translated_text_over_a_retained_inline_image_is_rejected() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let audit = audit_publication_ink(
            &input,
            &trace(r#"<fill_image transform="10 0 0 -10 20 80"/>"#),
        )
        .unwrap();
        assert!(kinds(&audit).contains(&InkViolationKind::RetainedInkCollision));
    }

    #[test]
    fn detached_formula_ink_is_rejected() {
        let components = serde_json::json!([
            {
                "kind": "source_text_replay",
                "ownership_group": 1,
                "bounds": {"left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0},
                "glyphs": [{
                    "unicode": "中",
                    "baseline_origin": {"x": 20.0, "y": 20.0},
                    "ink_bounds": {
                        "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
                    }
                }]
            },
            {
                "kind": "vector_path",
                "ownership_group": 1,
                "bounds": {"left": 20.0, "bottom": 21.5, "right": 30.0, "top": 22.5}
            }
        ]);
        let trace = trace(
            r#"<stroke_path linewidth="1" transform="1 0 0 1 0 0">
            <moveto x="20" y="60"/><lineto x="30" y="60"/></stroke_path>"#,
        );
        let audit = audit_publication_ink(&il(components), &trace).unwrap();
        assert!(kinds(&audit).contains(&InkViolationKind::MissingOutputInk));
    }

    #[test]
    fn bilingual_mode_audits_the_translated_page() {
        let input = il(serde_json::json!([text_component(serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        }))]));
        let trace = format!(
            "<document><page number=\"1\" mediabox=\"0 0 100 100\"></page>{}</document>",
            trace("")
                .trim_start_matches("<document>")
                .trim_end_matches("</document>")
        );
        let audit = audit_publication_ink(&input, &trace).unwrap();
        assert_eq!(audit.output_page_mode, "bilingual");
        assert_eq!(audit.violation_count(), 0);
    }

    #[test]
    fn cross_paragraph_collision_uses_glyph_ink_not_line_hulls() {
        let publication = |reading_order, glyph_bounds| {
            serde_json::from_value::<PublicationInk>(serde_json::json!({
                "page_index": 0,
                "reading_order": reading_order,
                "crop_box": {"left": 0.0, "bottom": 0.0, "right": 100.0, "top": 100.0},
                "admissible_container": {
                    "left": 10.0, "bottom": 10.0, "right": 90.0, "top": 90.0
                },
                "components": [{
                    "kind": "translated_text",
                    "ownership_group": 0,
                    "bounds": {
                        "left": 10.0, "bottom": 18.0, "right": 90.0, "top": 25.0
                    },
                    "glyphs": [{
                        "unicode": "中",
                        "baseline_origin": {"x": 20.0, "y": 20.0},
                        "ink_bounds": glyph_bounds
                    }]
                }]
            }))
            .unwrap()
        };
        let left_bounds = serde_json::json!({
            "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
        });
        let right_bounds = serde_json::json!({
            "left": 50.0, "bottom": 18.0, "right": 60.0, "top": 25.0
        });
        let publications = [publication(0, left_bounds), publication(1, right_bounds)];
        let mut violations = Vec::new();

        check_component_collisions(&publications, &mut violations);

        assert!(violations.is_empty());

        let overlapping = [
            publication(
                0,
                serde_json::json!({
                    "left": 19.0, "bottom": 18.0, "right": 30.0, "top": 25.0
                }),
            ),
            publication(
                1,
                serde_json::json!({
                    "left": 25.0, "bottom": 18.0, "right": 35.0, "top": 25.0
                }),
            ),
        ];
        check_component_collisions(&overlapping, &mut violations);
        assert!(
            violations
                .iter()
                .any(|value| value.kind == InkViolationKind::CrossParagraphCollision)
        );
    }
}
