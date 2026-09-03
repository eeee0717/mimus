use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mimus_quality_contract::{conserved_tokens, title_author_band};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u32 = 2;

#[derive(Parser)]
#[command(about = "Measure mimus output quality from public artifacts")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Measure(MeasureArgs),
}

#[derive(clap::Args)]
struct MeasureArgs {
    #[arg(long)]
    ndjson: PathBuf,
    #[arg(long)]
    debug_dir: PathBuf,
    #[arg(long)]
    input_pdf: PathBuf,
    #[arg(long)]
    output_pdf: PathBuf,
    #[arg(long)]
    json_out: PathBuf,
    #[arg(long)]
    markdown_out: PathBuf,
    #[arg(long, default_value_t = 72)]
    render_dpi: u32,
    #[arg(long, value_enum, default_value_t = EvaluationProfile::Real)]
    evaluation_profile: EvaluationProfile,
    #[arg(long)]
    glossary: Option<PathBuf>,
    #[arg(long)]
    semantic_evaluation: Option<PathBuf>,
    #[arg(long)]
    process_log: Option<PathBuf>,
    #[arg(long)]
    resource_usage: Option<PathBuf>,
    #[arg(long = "confirmed-critical")]
    confirmed_criticals: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum EvaluationProfile {
    Real,
    ConservingFake,
    LegacyFake,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
struct Rect {
    left: f64,
    bottom: f64,
    right: f64,
    top: f64,
}

#[derive(Debug, Deserialize)]
struct Il {
    pages: Vec<Page>,
    #[serde(default)]
    publication_ink: Vec<PublicationOwner>,
}

#[derive(Debug, Deserialize)]
struct PublicationOwner {
    page_index: usize,
    reading_order: usize,
    admissible_container: Rect,
}

#[derive(Debug, Deserialize)]
struct Page {
    index: usize,
    #[serde(default)]
    geometry: PageGeometry,
    paragraphs: Vec<Paragraph>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct PageGeometry {
    height: f64,
}

#[derive(Debug, Deserialize)]
struct Paragraph {
    reading_order: usize,
    bounds: Rect,
    text: Text,
    #[serde(default)]
    translated_text: Option<String>,
    #[serde(default)]
    translation_conservation: Option<TranslationConservationEvidence>,
    #[serde(default)]
    preserved: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TranslationConservationEvidence {
    request_sha256: String,
    response_sha256: String,
    source_token_types: usize,
    target_token_types: usize,
    source_tokens: Vec<ConservedTokenCount>,
    target_tokens: Vec<ConservedTokenCount>,
}

impl TranslationConservationEvidence {
    fn is_complete(&self) -> bool {
        valid_sha256(&self.request_sha256)
            && valid_sha256(&self.response_sha256)
            && self.source_token_types == self.source_tokens.len()
            && self.target_token_types == self.target_tokens.len()
    }
}

#[derive(Debug, Deserialize)]
struct ConservedTokenCount {
    token: String,
    occurrences: usize,
}

#[derive(Debug, Deserialize)]
struct Text {
    chars: Vec<Char>,
}

#[derive(Debug, Deserialize)]
struct Char {
    unicode: Option<String>,
    #[serde(default = "default_true")]
    visible: bool,
    #[serde(default)]
    implicit_space_before: bool,
    #[serde(default)]
    code: Option<u32>,
    font_size: f64,
    baseline_origin: Point,
    #[serde(default)]
    #[serde(rename = "box")]
    box_: Rect,
    #[serde(default)]
    visual_bbox: Option<Rect>,
    #[serde(default)]
    font: Option<Value>,
    #[serde(default)]
    passthrough: Option<Value>,
    #[serde(default)]
    text_transform: Option<TextTransform>,
    #[serde(default)]
    layout: Option<Layout>,
}

#[derive(Debug, Deserialize)]
struct TextTransform {
    kind: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct Point {
    #[serde(default)]
    x: f64,
    y: f64,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Layout {
    #[serde(default)]
    label: String,
    #[serde(default)]
    reading_order: usize,
    #[serde(default)]
    bounds: Rect,
    #[serde(default)]
    source: String,
    policy: String,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    evaluation_profile: EvaluationProfile,
    input_sha256: String,
    output_sha256: String,
    output_characters: usize,
    dimensions: BTreeMap<String, Dimension>,
    total_score: f64,
    conclusion: Conclusion,
    applicability: BTreeMap<String, Applicability>,
    process: ProcessMetrics,
    semantic_evaluation: Option<Value>,
    evidence: Evidence,
}

#[derive(Debug, Serialize)]
struct Conclusion {
    status: &'static str,
    confirmed_criticals: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Applicability {
    status: &'static str,
    reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct ProcessMetrics {
    formula_ids: Vec<&'static str>,
    terminal_result: bool,
    internal_errors: usize,
    eligible_paragraphs: usize,
    typed_degraded_paragraphs: usize,
    translation_calls: Option<usize>,
    translation_calls_per_eligible_paragraph: Option<f64>,
    term_extraction_calls: Option<usize>,
    retry_diagnostics: usize,
    retry_rate: Option<f64>,
    suspicious_echoes: usize,
    echo_rate: f64,
    cache_hits: usize,
    cache_misses: usize,
    cache_hit_rate: Option<f64>,
    wall_time_seconds: Option<f64>,
    peak_rss_bytes: Option<u64>,
    per_page_timing: Option<Value>,
}

#[derive(Debug, Serialize)]
struct Dimension {
    formula_ids: Vec<&'static str>,
    weighted_errors: f64,
    errors_per_1000_output_characters: f64,
    score: f64,
    measurements: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct Evidence {
    qpdf_input_ok: bool,
    qpdf_output_ok: bool,
    input_is_output_prefix: bool,
    page_count_equal: bool,
    non_text_object_counts_equal: bool,
    non_text_pixel_fidelity: Option<f64>,
}

fn main() -> Result<()> {
    let Cli { command } = Cli::parse();
    match command {
        Commands::Measure(args) => measure(args),
    }
}

fn measure(args: MeasureArgs) -> Result<()> {
    let before: Il = read_json(args.debug_dir.join("03-paragraph_find.il.json"))?;
    let styled: Il = read_json(args.debug_dir.join("04-styles_and_formulas.il.json"))?;
    let translated: Il = read_json(args.debug_dir.join("06-translate.il.json"))?;
    let typeset: Il = read_json(args.debug_dir.join("07-typeset.il.json"))?;
    let write_path = args.debug_dir.join("09-write.il.json");
    let write: Il = read_json(write_path.clone())?;
    let events = read_ndjson(&args.ndjson)?;
    let glossary = args.glossary.as_deref().map(read_glossary).transpose()?;
    let semantic_evaluation = args
        .semantic_evaluation
        .as_ref()
        .map(|path| read_json(path.clone()))
        .transpose()?;
    let output_chars = extracted_character_count(&args.output_pdf)?;
    let denominator = output_chars.max(1) as f64;
    let direct_content_objects = qpdf_direct_content_objects(&args.input_pdf)?;
    let ink_audit = scorecard::audit_publication_ink_paths(&write_path, &args.output_pdf)?;

    let mut dimensions = BTreeMap::new();
    dimensions.insert(
        "coverage_gap".into(),
        coverage(&before, &translated, &typeset, denominator),
    );
    dimensions.insert(
        "overtranslation".into(),
        overtranslation(&before, &translated, denominator),
    );
    dimensions.insert(
        "mistranslation_risk".into(),
        risk(
            &styled,
            &translated,
            &typeset,
            &events,
            args.evaluation_profile,
            glossary.as_ref(),
            &direct_content_objects,
            &args.input_pdf,
            &args.output_pdf,
            denominator,
        ),
    );
    dimensions.insert(
        "layout_drift".into(),
        layout(
            &before,
            &styled,
            &translated,
            &typeset,
            &events,
            &args.output_pdf,
            &ink_audit,
            denominator,
        ),
    );
    dimensions.insert(
        "typesetting_lint".into(),
        lint(&translated, &events, &args.output_pdf, denominator),
    );

    let input_bytes = fs::read(&args.input_pdf)?;
    let output_bytes = fs::read(&args.output_pdf)?;
    let qpdf_input_ok = qpdf_check(&args.input_pdf);
    let qpdf_output_ok = qpdf_check(&args.output_pdf);
    let input_pages = qpdf_pages(&args.input_pdf);
    let output_pages = qpdf_pages(&args.output_pdf);
    let input_counts = qpdf_non_text_counts(&args.input_pdf)?;
    let output_counts = qpdf_non_text_counts(&args.output_pdf)?;
    let masks = translated_masks(&translated);
    let pixel_fidelity = Some(pixel_fidelity(
        &args.input_pdf,
        &args.output_pdf,
        &masks,
        args.render_dpi,
    )?);
    let evidence = Evidence {
        qpdf_input_ok,
        qpdf_output_ok,
        input_is_output_prefix: output_bytes.starts_with(&input_bytes),
        page_count_equal: input_pages.is_some() && input_pages == output_pages,
        non_text_object_counts_equal: input_counts == output_counts,
        non_text_pixel_fidelity: pixel_fidelity,
    };
    dimensions.insert(
        "structural_fidelity".into(),
        structure(&before, &write, &evidence, denominator),
    );
    let total_score =
        round6(dimensions.values().map(|d| d.score).sum::<f64>() / dimensions.len() as f64);
    let mut applicability = BTreeMap::new();
    let conservation_applicable = !matches!(args.evaluation_profile, EvaluationProfile::LegacyFake);
    applicability.insert(
        "numeric_unit_reference_conservation".into(),
        applicability_value(
            conservation_applicable,
            "legacy fake responses intentionally discard source tokens",
        ),
    );
    applicability.insert(
        "terminology_consistency".into(),
        if !conservation_applicable {
            applicability_value(
                false,
                "legacy fake responses intentionally discard source terms",
            )
        } else if glossary.is_none() {
            applicability_value(false, "no glossary supplied")
        } else {
            applicability_value(true, "")
        },
    );
    applicability.insert(
        "semantic_evaluation".into(),
        applicability_value(
            semantic_evaluation.is_some(),
            "no COMETKiwi sidecar supplied",
        ),
    );
    let process = process_metrics(
        &before,
        &events,
        args.process_log.as_deref(),
        args.resource_usage.as_deref(),
    )?;
    let conclusion = Conclusion {
        status: if args.confirmed_criticals.is_empty() {
            "automatic_score_only"
        } else {
            "blocked_by_confirmed_critical"
        },
        confirmed_criticals: args.confirmed_criticals,
    };
    let report = Report {
        schema_version: SCHEMA_VERSION,
        evaluation_profile: args.evaluation_profile,
        input_sha256: sha256(&input_bytes),
        output_sha256: sha256(&output_bytes),
        output_characters: output_chars,
        dimensions,
        total_score,
        conclusion,
        applicability,
        process,
        semantic_evaluation,
        evidence,
    };
    let json = serde_json::to_string_pretty(&report)? + "\n";
    write_atomic(&args.json_out, json.as_bytes())?;
    write_atomic(&args.markdown_out, markdown(&report).as_bytes())?;
    let _ = write.pages.len();
    Ok(())
}

fn coverage(before: &Il, translated: &Il, typeset: &Il, denominator: f64) -> Dimension {
    let mut eligible = 0usize;
    let mut covered = 0usize;
    let mut translated_han = 0usize;
    let mut published_han = 0usize;
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for page in &before.pages {
        for source in &page.paragraphs {
            if !is_translatable(source) {
                continue;
            }
            eligible += 1;
            let translated_paragraph = find_paragraph(translated, page.index, source.reading_order);
            let typeset_paragraph = find_paragraph(typeset, page.index, source.reading_order);
            translated_han += translated_paragraph
                .and_then(|p| p.translated_text.as_deref())
                .map(count_han)
                .unwrap_or(0);
            if translated_paragraph
                .is_some_and(|p| p.translated_text.is_some() && p.preserved.is_none())
                && typeset_paragraph
                    .is_some_and(|p| p.translated_text.is_some() && p.preserved.is_none())
            {
                covered += 1;
                published_han += typeset_paragraph
                    .and_then(|p| p.translated_text.as_deref())
                    .map(count_han)
                    .unwrap_or(0);
            } else if let Some(reason) = typeset_paragraph
                .and_then(preserved_reason)
                .or_else(|| translated_paragraph.and_then(preserved_reason))
            {
                *reasons.entry(reason).or_default() += 1;
            }
        }
    }
    let missing = eligible.saturating_sub(covered);
    let weighted = missing as f64 * 5.0;
    dimension(
        &["COV-01", "COV-02", "COV-03"],
        weighted,
        denominator,
        BTreeMap::from([
            ("eligible_paragraphs".into(), eligible.into()),
            ("translated_paragraphs".into(), covered.into()),
            (
                "paragraph_coverage".into(),
                ratio(covered as f64, eligible as f64).into(),
            ),
            (
                "han_weighted_coverage".into(),
                ratio(published_han as f64, translated_han as f64).into(),
            ),
            ("translated_han".into(), translated_han.into()),
            ("published_han".into(), published_han.into()),
            (
                "preserved_reasons".into(),
                serde_json::to_value(reasons).unwrap(),
            ),
        ]),
    )
}

fn overtranslation(before: &Il, after: &Il, denominator: f64) -> Dimension {
    let mut policy = 0usize;
    let mut superscript = 0usize;
    let mut numbering = 0usize;
    let mut blank = 0usize;
    for (a, b) in paragraph_pairs(before, after) {
        if b.translated_text
            .as_deref()
            .is_none_or(|t| text_equivalent(t, &source_text(a)))
        {
            continue;
        }
        if !is_translatable(a) {
            policy += 1;
        }
        if looks_like_superscript(a) {
            superscript += 1;
        }
        let text = source_text(a);
        if is_numbering(&text) {
            numbering += 1;
        }
        if text.trim().is_empty() {
            blank += 1;
        }
    }
    let weighted = (policy * 10 + superscript * 5 + numbering * 5 + blank * 5) as f64;
    dimension(
        &["OVR-01", "OVR-02", "OVR-03", "OVR-04"],
        weighted,
        denominator,
        values(&[
            ("policy_passthrough_changed", policy),
            ("superscript_candidates", superscript),
            ("numbering_candidates", numbering),
            ("blank_content_changed", blank),
        ]),
    )
}

#[derive(Debug, Deserialize)]
struct Glossary {
    version: u32,
    #[serde(default)]
    terms: Vec<GlossaryTerm>,
}

#[derive(Debug, Deserialize)]
struct GlossaryTerm {
    source: String,
    target: String,
}

#[allow(clippy::too_many_arguments)]
fn risk(
    styled: &Il,
    translated: &Il,
    published: &Il,
    events: &[Value],
    profile: EvaluationProfile,
    glossary: Option<&Glossary>,
    direct_content_objects: &BTreeMap<usize, BTreeSet<u32>>,
    input_pdf: &Path,
    output_pdf: &Path,
    denominator: f64,
) -> Dimension {
    let weak = styled
        .pages
        .iter()
        .flat_map(|p| &p.paragraphs)
        .filter(|p| is_translatable(p) && p.text.chars.iter().any(|c| c.unicode.is_none()))
        .count();
    let placeholders = count_diagnostic(events, "placeholder_violation");
    let echoes = count_summary_array(events, "suspicious_echoes");
    let conservation = conservation_measurement(styled, translated, Some(direct_content_objects));
    let formula = formula_completeness(styled, translated);
    let orphan_ink = orphan_source_ink(styled, published, input_pdf, output_pdf);
    let orphan_ink_violations = orphan_ink.as_ref().map_or(0, |value| value.violations);
    let rigid_body = formula_rigid_body_integrity(styled, published, input_pdf, output_pdf);
    let rigid_body_violations = rigid_body.as_ref().map_or(0, |value| value.violations);
    let terminology = glossary
        .map(|g| terminology_measurement(styled, translated, g, Some(direct_content_objects)));
    let conservation_applicable = !matches!(profile, EvaluationProfile::LegacyFake);
    let missing_conserved = if conservation_applicable {
        conservation.missing_occurrences
    } else {
        0
    };
    let terminology_violations = if conservation_applicable {
        terminology.as_ref().map_or(0, |m| m.violations)
    } else {
        0
    };
    let weighted = (weak * 5 + placeholders * 10 + echoes * 5) as f64
        + missing_conserved as f64 * 10.0
        + terminology_violations as f64 * 5.0
        + formula.violations as f64 * 10.0
        + orphan_ink_violations as f64 * 10.0
        + rigid_body_violations as f64 * 10.0;
    let mut measurements = values(&[
        ("weak_reliability_paragraphs", weak),
        ("placeholder_violations", placeholders),
        ("suspicious_echoes", echoes),
    ]);
    measurements.insert(
        "numeric_unit_reference_conservation".into(),
        if conservation_applicable {
            serde_json::to_value(&conservation).unwrap()
        } else {
            not_applicable("legacy fake responses intentionally discard source tokens")
        },
    );
    measurements.insert(
        "terminology_consistency".into(),
        if !conservation_applicable {
            not_applicable("legacy fake responses intentionally discard source terms")
        } else if let Some(measurement) = terminology {
            serde_json::to_value(measurement).unwrap()
        } else {
            not_applicable("no glossary supplied")
        },
    );
    measurements.insert(
        "formula_unit_completeness_proxy".into(),
        serde_json::to_value(formula).unwrap(),
    );
    measurements.insert(
        "orphan_source_ink".into(),
        orphan_ink
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or_else(|| not_applicable("mutool trace evidence unavailable")),
    );
    measurements.insert(
        "formula_rigid_body_integrity".into(),
        rigid_body
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or_else(|| not_applicable("mutool trace evidence unavailable")),
    );
    dimension(
        &[
            "RSK-01", "RSK-02", "RSK-03", "CON-01", "CON-02", "FOR-01", "FOR-04", "FOR-05",
        ],
        weighted,
        denominator,
        measurements,
    )
}

#[allow(clippy::too_many_arguments)]
fn layout(
    before: &Il,
    styled: &Il,
    translated: &Il,
    after: &Il,
    events: &[Value],
    output_pdf: &Path,
    ink_audit: &scorecard::InkAudit,
    denominator: f64,
) -> Dimension {
    let mut offsets = Vec::new();
    let mut ious = Vec::new();
    let mut font_ratios = Vec::new();
    for (a, b) in paragraph_pairs(before, after) {
        if b.translated_text.is_none() {
            continue;
        }
        offsets.push(
            ((a.bounds.left - b.bounds.left).powi(2) + (a.bounds.bottom - b.bounds.bottom).powi(2))
                .sqrt(),
        );
        ious.push(iou(a.bounds, b.bounds));
        let af = median_font(a);
        let bf = median_font(b);
        if af > 0.0 && bf > 0.0 {
            font_ratios.push(bf / af);
        }
    }
    let expansions = count_diagnostic(events, "single_line_bounds_expanded")
        + count_diagnostic(events, "multi_line_bounds_expanded");
    let drifted =
        offsets.iter().filter(|v| **v > 1.0).count() + ious.iter().filter(|v| **v < 0.8).count();
    let continuity = formula_continuity(styled, translated, output_pdf);
    let continuity_violations = continuity.as_ref().map_or(0, |m| m.excessive_gap_count);
    let hole_count = continuity.as_ref().map_or(0, |m| m.unexplained_hole_count);
    let weighted = (drifted * 3 + expansions) as f64
        + continuity_violations as f64 * 5.0
        + hole_count as f64 * 5.0
        + ink_audit.violation_count() as f64 * 10.0;
    let mut m = BTreeMap::new();
    m.insert("median_offset_pt".into(), median(&mut offsets).into());
    m.insert("median_iou".into(), median(&mut ious).into());
    m.insert("median_font_scale".into(), median(&mut font_ratios).into());
    m.insert("bounds_expansions".into(), expansions.into());
    m.insert(
        "final_ink_geometry".into(),
        serde_json::to_value(ink_audit).unwrap(),
    );
    m.insert(
        "formula_neighbor_continuity".into(),
        continuity
            .as_ref()
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or_else(|| not_applicable("mutool stext evidence unavailable")),
    );
    m.insert(
        "unexplained_inline_holes".into(),
        continuity
            .map(|value| {
                serde_json::json!({
                    "status": "applicable",
                    "count": value.unexplained_hole_count,
                    "area_pt2": value.unexplained_hole_area_pt2,
                })
            })
            .unwrap_or_else(|| not_applicable("mutool stext evidence unavailable")),
    );
    dimension(
        &[
            "LAY-01", "LAY-02", "LAY-03", "LAY-04", "FOR-02", "FOR-03", "INK-01",
        ],
        weighted,
        denominator,
        m,
    )
}

fn lint(after: &Il, events: &[Value], output_pdf: &Path, denominator: f64) -> Dimension {
    let texts: Vec<&str> = after
        .pages
        .iter()
        .flat_map(|p| &p.paragraphs)
        .filter_map(|p| p.translated_text.as_deref())
        .collect();
    let paragraph_kinsoku = texts
        .iter()
        .filter(|t| {
            t.starts_with(mimus_quality_contract::forbidden_line_start)
                || t.ends_with(mimus_quality_contract::forbidden_line_end)
        })
        .count();
    let kinsoku = output_line_kinsoku_violations(output_pdf).unwrap_or(paragraph_kinsoku);
    let isolated = texts
        .iter()
        .filter(|t| t.chars().count() == 1 && t.chars().all(is_punctuation))
        .count();
    let residues = texts
        .iter()
        .map(|t| {
            ["{v", "{l", "<b"]
                .iter()
                .filter(|x| t.contains(**x))
                .count()
        })
        .sum::<usize>();
    let whitespace = texts
        .iter()
        .filter(|t| {
            t.contains("  ")
                || t.starts_with(char::is_whitespace)
                || t.ends_with(char::is_whitespace)
        })
        .count();
    let echoes = count_summary_array(events, "suspicious_echoes");
    let weighted = (kinsoku * 3 + isolated + residues * 10 + whitespace + echoes) as f64;
    dimension(
        &["LNT-01", "LNT-02", "LNT-03", "LNT-04", "LNT-05"],
        weighted,
        denominator,
        values(&[
            ("kinsoku_violations", kinsoku),
            ("isolated_punctuation", isolated),
            ("placeholder_residue", residues),
            ("abnormal_whitespace", whitespace),
            ("english_residual_proxy", echoes),
        ]),
    )
}

fn output_line_kinsoku_violations(output_pdf: &Path) -> Option<usize> {
    let temp = tempfile::NamedTempFile::new().ok()?;
    let output = Command::new("mutool")
        .args(["draw", "-q", "-F", "stext.json", "-o"])
        .arg(temp.path())
        .arg(output_pdf)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let document: StextDocument = serde_json::from_slice(&fs::read(temp.path()).ok()?).ok()?;
    Some(
        document
            .pages
            .iter()
            .flat_map(|page| &page.blocks)
            .flat_map(|block| &block.lines)
            .filter(|line| {
                let text = line.text.trim();
                text.starts_with(mimus_quality_contract::forbidden_line_start)
                    || text.ends_with(mimus_quality_contract::forbidden_line_end)
            })
            .count(),
    )
}

fn structure(before: &Il, write: &Il, e: &Evidence, denominator: f64) -> Dimension {
    let binaries = [
        e.qpdf_input_ok,
        e.qpdf_output_ok,
        e.input_is_output_prefix,
        e.page_count_equal,
        e.non_text_object_counts_equal,
        e.non_text_pixel_fidelity.is_some(),
    ];
    let failed = binaries.iter().filter(|v| !**v).count();
    let pixel_error = e
        .non_text_pixel_fidelity
        .map_or(1000.0, |v| (1.0 - v).max(0.0) * 1000.0);
    let title_author = title_author_conservation(before, write);
    let title_author_failures = title_author.as_ref().map_or(0, |m| m.failures);
    let weighted = failed as f64 * 100.0 + pixel_error * 10.0 + title_author_failures as f64 * 10.0;
    let mut m = BTreeMap::new();
    m.insert("binary_checks_failed".into(), failed.into());
    m.insert(
        "masked_non_text_pixel_fidelity".into(),
        e.non_text_pixel_fidelity.into(),
    );
    m.insert(
        "title_author_conservation".into(),
        title_author
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or_else(|| {
                not_applicable("page 0 title and lower author-block anchors not both present")
            }),
    );
    dimension(
        &["STR-01", "STR-02", "STR-03", "STR-04", "STR-05"],
        weighted,
        denominator,
        m,
    )
}

fn dimension(
    ids: &[&'static str],
    weighted: f64,
    denominator: f64,
    measurements: BTreeMap<String, Value>,
) -> Dimension {
    let rate = weighted * 1000.0 / denominator;
    Dimension {
        formula_ids: ids.to_vec(),
        weighted_errors: round6(weighted),
        errors_per_1000_output_characters: round6(rate),
        score: round6((100.0 - rate).clamp(0.0, 100.0)),
        measurements,
    }
}

#[derive(Debug, Serialize)]
struct ConservationMeasurement {
    status: &'static str,
    checked_paragraphs: usize,
    source_occurrences: usize,
    preserved_occurrences: usize,
    missing_occurrences: usize,
    conservation_rate: f64,
    violations: Vec<TokenViolation>,
}

#[derive(Debug, Serialize)]
struct TokenViolation {
    page_index: usize,
    reading_order: usize,
    token: String,
    missing: usize,
}

fn conservation_measurement(
    before: &Il,
    after: &Il,
    direct_content_objects: Option<&BTreeMap<usize, BTreeSet<u32>>>,
) -> ConservationMeasurement {
    let mut checked = 0;
    let mut source_occurrences = 0;
    let mut missing_occurrences = 0;
    let mut violations = Vec::new();
    for page in &before.pages {
        for source in &page.paragraphs {
            if !is_translatable(source) {
                continue;
            }
            let Some(translated) = find_paragraph(after, page.index, source.reading_order) else {
                continue;
            };
            let Some(translation) = translated.translated_text.as_deref() else {
                continue;
            };
            checked += 1;
            let (counts, translated_counts) = translated
                .translation_conservation
                .as_ref()
                .filter(|evidence| evidence.is_complete())
                .map(|evidence| {
                    (
                        evidence_token_counts(&evidence.source_tokens),
                        evidence_token_counts(&evidence.target_tokens),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        token_multiset(conserved_tokens(&translate_source_text_for_page(
                            source,
                            direct_content_objects.and_then(|objects| objects.get(&page.index)),
                        ))),
                        token_multiset(conserved_tokens(translation)),
                    )
                });
            for (token, expected) in counts {
                source_occurrences += expected;
                let found = translated_counts
                    .get(&token)
                    .copied()
                    .unwrap_or(0)
                    .min(expected);
                let missing = expected - found;
                if missing > 0 {
                    missing_occurrences += missing;
                    violations.push(TokenViolation {
                        page_index: page.index,
                        reading_order: source.reading_order,
                        token,
                        missing,
                    });
                }
            }
        }
    }
    ConservationMeasurement {
        status: "applicable",
        checked_paragraphs: checked,
        source_occurrences,
        preserved_occurrences: source_occurrences.saturating_sub(missing_occurrences),
        missing_occurrences,
        conservation_rate: ratio(
            source_occurrences.saturating_sub(missing_occurrences) as f64,
            source_occurrences as f64,
        ),
        violations,
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn evidence_token_counts(tokens: &[ConservedTokenCount]) -> BTreeMap<String, usize> {
    tokens.iter().fold(BTreeMap::new(), |mut counts, entry| {
        *counts.entry(entry.token.clone()).or_default() += entry.occurrences;
        counts
    })
}

fn token_multiset(tokens: Vec<String>) -> BTreeMap<String, usize> {
    tokens
        .into_iter()
        .fold(BTreeMap::new(), |mut counts, token| {
            *counts.entry(token).or_default() += 1;
            counts
        })
}

#[derive(Debug, Serialize)]
struct TerminologyMeasurement {
    status: &'static str,
    glossary_terms: usize,
    source_occurrences: usize,
    canonical_occurrences: usize,
    violations: usize,
    consistency_rate: f64,
    violating_terms: Vec<String>,
}

fn terminology_measurement(
    before: &Il,
    after: &Il,
    glossary: &Glossary,
    direct_content_objects: Option<&BTreeMap<usize, BTreeSet<u32>>>,
) -> TerminologyMeasurement {
    let mut source_occurrences = 0;
    let mut canonical_occurrences = 0;
    let mut violations = 0;
    let mut violating_terms = Vec::new();
    for term in &glossary.terms {
        let mut term_source = 0;
        let mut term_canonical = 0;
        for page in &before.pages {
            for source in &page.paragraphs {
                let source_count = translate_source_text_for_page(
                    source,
                    direct_content_objects.and_then(|objects| objects.get(&page.index)),
                )
                .match_indices(&term.source)
                .count();
                if source_count == 0 {
                    continue;
                }
                term_source += source_count;
                term_canonical += find_paragraph(after, page.index, source.reading_order)
                    .and_then(|paragraph| paragraph.translated_text.as_deref())
                    .map_or(0, |text| text.match_indices(&term.target).count())
                    .min(source_count);
            }
        }
        source_occurrences += term_source;
        canonical_occurrences += term_canonical;
        if term_canonical < term_source {
            violations += term_source - term_canonical;
            violating_terms.push(term.source.clone());
        }
    }
    TerminologyMeasurement {
        status: "applicable",
        glossary_terms: glossary.terms.len(),
        source_occurrences,
        canonical_occurrences,
        violations,
        consistency_rate: ratio(canonical_occurrences as f64, source_occurrences as f64),
        violating_terms,
    }
}

#[derive(Debug, Serialize)]
struct FormulaCompleteness {
    status: &'static str,
    checked_formula_paragraphs: usize,
    unbalanced_delimiter_paragraphs: usize,
    adjacent_fragment_count: usize,
    violations: usize,
    evidence: Vec<FormulaEvidence>,
}

#[derive(Debug, Serialize)]
struct FormulaEvidence {
    page_index: usize,
    reading_order: usize,
    kind: &'static str,
    text: String,
}

fn formula_completeness(before: &Il, after: &Il) -> FormulaCompleteness {
    let mut checked = 0;
    let mut unbalanced = 0;
    let mut fragments = 0;
    let mut evidence = Vec::new();
    for page in &before.pages {
        for source in &page.paragraphs {
            if !source.text.chars.iter().any(is_inline_formula) {
                continue;
            }
            checked += 1;
            if let Some(text) = find_paragraph(after, page.index, source.reading_order)
                .and_then(|paragraph| paragraph.translated_text.as_deref())
                && !delimiters_balanced(text)
            {
                unbalanced += 1;
                evidence.push(FormulaEvidence {
                    page_index: page.index,
                    reading_order: source.reading_order,
                    kind: "unbalanced_delimiter",
                    text: text.to_string(),
                });
            }
            for fragment in formula_adjacent_fragments(source) {
                fragments += 1;
                evidence.push(FormulaEvidence {
                    page_index: page.index,
                    reading_order: source.reading_order,
                    kind: "translate_fragment_adjacent_to_formula",
                    text: fragment,
                });
            }
        }
    }
    FormulaCompleteness {
        status: "applicable",
        checked_formula_paragraphs: checked,
        unbalanced_delimiter_paragraphs: unbalanced,
        adjacent_fragment_count: fragments,
        violations: unbalanced + fragments,
        evidence,
    }
}

fn delimiters_balanced(text: &str) -> bool {
    let mut stack = Vec::new();
    for c in text.chars() {
        if "([{（【".contains(c) {
            stack.push(c);
        } else if ")] }）】".replace(' ', "").contains(c) {
            let expected = match c {
                ')' => '(',
                ']' => '[',
                '}' => '{',
                '）' => '（',
                '】' => '【',
                _ => unreachable!(),
            };
            if stack.pop() != Some(expected) {
                return false;
            }
        }
    }
    stack.is_empty()
}

fn formula_adjacent_fragments(paragraph: &Paragraph) -> Vec<String> {
    let chars = &paragraph.text.chars;
    let mut out = Vec::new();
    for i in 0..chars.len() {
        if !is_inline_formula(&chars[i]) {
            continue;
        }
        if i + 1 < chars.len()
            && !is_inline_formula(&chars[i + 1])
            && let Some(fragment) = adjacent_fragment(chars, i + 1, 1)
        {
            out.push(fragment);
        }
        if i > 0
            && !is_inline_formula(&chars[i - 1])
            && let Some(fragment) = adjacent_fragment(chars, i - 1, -1)
        {
            out.push(fragment);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn adjacent_fragment(chars: &[Char], start: usize, direction: isize) -> Option<String> {
    let anchor = &chars[(start as isize - direction) as usize];
    let mut selected = Vec::new();
    let mut index = start as isize;
    let mut previous = anchor;
    while index >= 0 && (index as usize) < chars.len() && selected.len() < 8 {
        let current = &chars[index as usize];
        if is_inline_formula(current)
            || current
                .layout
                .as_ref()
                .is_none_or(|layout| layout.policy != "translate")
            || (current.baseline_origin.y - anchor.baseline_origin.y).abs()
                > anchor.font_size * 0.25
        {
            break;
        }
        let gap = if direction > 0 {
            current.box_.left - previous.box_.right
        } else {
            previous.box_.left - current.box_.right
        };
        if gap > anchor.font_size * 0.5 {
            break;
        }
        let value = current.unicode.as_deref().unwrap_or("");
        if value.chars().any(char::is_whitespace) {
            break;
        }
        selected.push(value.to_string());
        previous = current;
        index += direction;
    }
    if direction < 0 {
        selected.reverse();
    }
    let fragment = selected.concat();
    let suspicious = !fragment.is_empty()
        && fragment.chars().count() <= 8
        && (fragment.chars().any(|c| c.is_ascii_digit())
            || fragment
                .chars()
                .any(|c| ")] }".replace(' ', "").contains(c))
            || fragment == "model"
            || fragment.starts_with('_'));
    suspicious.then_some(fragment)
}

fn is_inline_formula(c: &Char) -> bool {
    c.layout
        .as_ref()
        .is_some_and(|layout| layout.label == "inline_formula")
}

#[derive(Debug, Serialize)]
struct FormulaContinuity {
    status: &'static str,
    formula_units: usize,
    matched_units: usize,
    measured_neighbor_gaps: usize,
    excessive_gap_count: usize,
    max_gap_pt: f64,
    unexplained_hole_count: usize,
    unexplained_hole_area_pt2: f64,
    evidence: Vec<GapEvidence>,
}

#[derive(Debug, Serialize)]
struct GapEvidence {
    page_index: usize,
    reading_order: usize,
    formula: String,
    gap_pt: f64,
    bound_pt: f64,
}

#[derive(Debug, Deserialize)]
struct StextDocument {
    pages: Vec<StextPage>,
}

#[derive(Debug, Deserialize)]
struct StextPage {
    blocks: Vec<StextBlock>,
}

#[derive(Debug, Deserialize)]
struct StextBlock {
    #[serde(default)]
    lines: Vec<StextLine>,
}

#[derive(Clone, Debug, Deserialize)]
struct StextLine {
    bbox: StextBox,
    text: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct StextBox {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct FormulaUnit<'a> {
    page_index: usize,
    reading_order: usize,
    paragraph: &'a Paragraph,
    text: String,
    expected_stext_y: f64,
    font_size: f64,
    expects_left_neighbor: bool,
    expects_right_neighbor: bool,
    source_bounds: Option<Rect>,
    source_glyphs: Vec<(char, Point)>,
    has_attached_source_radical: bool,
}

#[derive(Debug, Serialize)]
struct FormulaRigidBodyIntegrity {
    status: &'static str,
    checked_formula_units: usize,
    violations: usize,
    evidence: Vec<FormulaRigidBodyEvidence>,
}

#[derive(Debug, Serialize)]
struct FormulaRigidBodyEvidence {
    page_index: usize,
    reading_order: usize,
    formula: String,
    source_glyph_count: usize,
    source_ink_count: usize,
}

#[derive(Debug, Serialize)]
struct OrphanSourceInk {
    status: &'static str,
    checked_formula_units: usize,
    violations: usize,
    evidence: Vec<OrphanInkEvidence>,
}

#[derive(Debug, Serialize)]
struct OrphanInkEvidence {
    page_index: usize,
    reading_order: usize,
    formula: String,
    ink_kind: &'static str,
    source_bounds: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TraceInkKind {
    VectorPath,
    InlineImage,
}

#[derive(Clone, Copy, Debug)]
struct TraceInk {
    kind: TraceInkKind,
    bounds: Rect,
}

#[derive(Clone, Debug)]
struct TraceGlyph {
    unicode: char,
    origin: Point,
}

#[derive(Default)]
struct TracePage {
    height: f64,
    glyphs: Vec<TraceGlyph>,
    ink: Vec<TraceInk>,
}

fn orphan_source_ink(
    styled: &Il,
    published: &Il,
    input_pdf: &Path,
    output_pdf: &Path,
) -> Option<OrphanSourceInk> {
    let source = mutool_trace(input_pdf)?;
    let output = mutool_trace(output_pdf)?;
    orphan_source_ink_from_traces(styled, published, &source, &output).ok()
}

fn formula_rigid_body_integrity(
    styled: &Il,
    published: &Il,
    input_pdf: &Path,
    output_pdf: &Path,
) -> Option<FormulaRigidBodyIntegrity> {
    let source = mutool_trace(input_pdf)?;
    let output = mutool_trace(output_pdf)?;
    formula_rigid_body_integrity_from_traces(styled, published, &source, &output).ok()
}

fn formula_rigid_body_integrity_from_traces(
    styled: &Il,
    published: &Il,
    source_trace: &str,
    output_trace: &str,
) -> Result<FormulaRigidBodyIntegrity> {
    let source_pages = parse_trace(source_trace)?;
    let output_pages = parse_trace(output_trace)?;
    let units = formula_units(styled);
    let mut checked = 0;
    let mut evidence = Vec::new();
    for (unit_index, unit) in units.iter().enumerate() {
        let Some(published_paragraph) =
            find_paragraph(published, unit.page_index, unit.reading_order)
        else {
            continue;
        };
        if published_paragraph.translated_text.is_none() || published_paragraph.preserved.is_some()
        {
            continue;
        }
        let (Some(_bounds), Some(source_page), Some(output_page)) = (
            unit.source_bounds,
            source_pages.get(unit.page_index),
            output_pages.get(unit.page_index),
        ) else {
            continue;
        };
        let source_ink = uniquely_owned_formula_ink(&units, unit_index, source_page);
        if !unit.has_attached_source_radical && source_ink.is_empty() {
            continue;
        }
        if unit.source_glyphs.len() < 2
            || !unit.source_glyphs.iter().all(|(unicode, origin)| {
                source_page.glyphs.iter().any(|glyph| {
                    glyph.unicode == *unicode
                        && point_distance(glyph.origin, *origin) <= unit.font_size.max(1.0) * 0.08
                })
            })
        {
            continue;
        }
        checked += 1;
        let tolerance = unit.font_size.max(1.0) * 0.08;
        let (anchor_unicode, anchor_origin) = unit.source_glyphs[0];
        let output_owner = formula_output_owner(published, unit);
        let preserves_one_delta = output_page
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.unicode == anchor_unicode
                    && glyph.origin.x >= output_owner.left
                    && glyph.origin.x <= output_owner.right
                    && glyph.origin.y >= output_owner.bottom
                    && glyph.origin.y <= output_owner.top
            })
            .any(|anchor| {
                let delta = Point {
                    x: anchor.origin.x - anchor_origin.x,
                    y: anchor.origin.y - anchor_origin.y,
                };
                let glyphs_match = unit.source_glyphs.iter().all(|(unicode, origin)| {
                    let expected = Point {
                        x: origin.x + delta.x,
                        y: origin.y + delta.y,
                    };
                    output_page.glyphs.iter().any(|glyph| {
                        glyph.unicode == *unicode
                            && point_distance(glyph.origin, expected) <= tolerance
                    })
                });
                glyphs_match
                    && source_ink.iter().all(|ink| {
                        let expected = translated_rect(ink.bounds, delta.x, delta.y);
                        output_page.ink.iter().any(|candidate| {
                            candidate.kind == ink.kind
                                && rect_close(candidate.bounds, expected, 0.05)
                        })
                    })
            });
        if !preserves_one_delta {
            evidence.push(FormulaRigidBodyEvidence {
                page_index: unit.page_index,
                reading_order: unit.reading_order,
                formula: unit.text.clone(),
                source_glyph_count: unit.source_glyphs.len(),
                source_ink_count: source_ink.len(),
            });
        }
    }
    Ok(FormulaRigidBodyIntegrity {
        status: "applicable",
        checked_formula_units: checked,
        violations: evidence.len(),
        evidence,
    })
}

fn formula_output_owner(published: &Il, unit: &FormulaUnit<'_>) -> Rect {
    let mut owners = published.publication_ink.iter().filter(|owner| {
        owner.page_index == unit.page_index && owner.reading_order == unit.reading_order
    });
    let unique = owners.next().filter(|_| owners.next().is_none());
    expand_rect(
        unique.map_or(unit.paragraph.bounds, |owner| owner.admissible_container),
        unit.font_size * 0.5,
    )
}

fn uniquely_owned_formula_ink(
    units: &[FormulaUnit<'_>],
    unit_index: usize,
    page: &TracePage,
) -> Vec<TraceInk> {
    let unit = &units[unit_index];
    page.ink
        .iter()
        .copied()
        .filter(|ink| {
            let owners = units
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.page_index == unit.page_index
                        && std::ptr::eq(candidate.paragraph, unit.paragraph)
                        && formula_owns_trace_ink(candidate, ink)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            owners.as_slice() == [unit_index]
        })
        .collect()
}

fn formula_owns_trace_ink(unit: &FormulaUnit<'_>, ink: &TraceInk) -> bool {
    let Some(bounds) = unit.source_bounds else {
        return false;
    };
    let em = unit.font_size;
    if !em.is_finite() || em <= 0.0 {
        return false;
    }
    let width = ink.bounds.right - ink.bounds.left;
    let height = ink.bounds.top - ink.bounds.bottom;
    let unit_width = bounds.right - bounds.left;
    match ink.kind {
        TraceInkKind::VectorPath => {
            if width <= 0.01 || width <= height || width > em * 4.0 || width > unit_width + em {
                return false;
            }
            let overlap =
                (ink.bounds.right.min(bounds.right) - ink.bounds.left.max(bounds.left)).max(0.0);
            let comparable_width = width.min(unit_width);
            if comparable_width <= 0.01 || overlap < comparable_width * 0.5 {
                return false;
            }
            let y = (ink.bounds.bottom + ink.bounds.top) / 2.0;
            if y < bounds.bottom - em * 0.25 || y > bounds.top + em * 0.5 {
                return false;
            }
            let highest_baseline = unit
                .source_glyphs
                .iter()
                .map(|(_, origin)| origin.y)
                .fold(f64::NEG_INFINITY, f64::max);
            let caps_formula = y >= highest_baseline - em * 0.05 && y <= bounds.top + em * 0.5;
            let separates_formula_rows = unit
                .source_glyphs
                .iter()
                .any(|(_, origin)| origin.y > y + em * 0.05)
                && unit
                    .source_glyphs
                    .iter()
                    .any(|(_, origin)| origin.y < y - em * 0.05);
            caps_formula || separates_formula_rows
        }
        TraceInkKind::InlineImage => {
            if width <= 0.0
                || height <= 0.0
                || width > unit_width + em
                || height > bounds.top - bounds.bottom + em
            {
                return false;
            }
            let overlap_x =
                (ink.bounds.right.min(bounds.right) - ink.bounds.left.max(bounds.left)).max(0.0);
            let overlap_y =
                (ink.bounds.top.min(bounds.top) - ink.bounds.bottom.max(bounds.bottom)).max(0.0);
            overlap_x > 0.01 && overlap_y > 0.01
        }
    }
}

fn mutool_trace(path: &Path) -> Option<String> {
    let temp = tempfile::NamedTempFile::new().ok()?;
    let output = Command::new("mutool")
        .args(["draw", "-q", "-F", "trace", "-o"])
        .arg(temp.path())
        .arg(path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| fs::read_to_string(temp.path()).ok())
        .flatten()
}

fn orphan_source_ink_from_traces(
    styled: &Il,
    published: &Il,
    source_trace: &str,
    output_trace: &str,
) -> Result<OrphanSourceInk> {
    let source_pages = parse_trace(source_trace)?;
    let output_pages = parse_trace(output_trace)?;
    let units = formula_units(styled);
    let mut checked = 0;
    let mut evidence = Vec::new();
    for (unit_index, unit) in units.iter().enumerate() {
        let Some(published_paragraph) =
            find_paragraph(published, unit.page_index, unit.reading_order)
        else {
            continue;
        };
        if published_paragraph.translated_text.is_none() || published_paragraph.preserved.is_some()
        {
            continue;
        }
        checked += 1;
        let (Some(_formula_bounds), Some(source_page), Some(output_page)) = (
            unit.source_bounds,
            source_pages.get(unit.page_index),
            output_pages.get(unit.page_index),
        ) else {
            continue;
        };
        let source_formula_still_present = unit.source_glyphs.iter().all(|(unicode, origin)| {
            output_page.glyphs.iter().any(|glyph| {
                glyph.unicode == *unicode
                    && point_distance(glyph.origin, *origin) <= unit.font_size.max(1.0) * 0.08
            })
        });
        if source_formula_still_present {
            continue;
        }
        for ink in uniquely_owned_formula_ink(&units, unit_index, source_page) {
            if !output_page.ink.iter().any(|candidate| {
                candidate.kind == ink.kind && rect_close(candidate.bounds, ink.bounds, 0.05)
            }) {
                continue;
            }
            evidence.push(OrphanInkEvidence {
                page_index: unit.page_index,
                reading_order: unit.reading_order,
                formula: unit.text.clone(),
                ink_kind: match ink.kind {
                    TraceInkKind::VectorPath => "vector_path",
                    TraceInkKind::InlineImage => "inline_image",
                },
                source_bounds: ink.bounds,
            });
        }
    }
    evidence.sort_by(|left, right| {
        (left.page_index, left.reading_order, left.ink_kind).cmp(&(
            right.page_index,
            right.reading_order,
            right.ink_kind,
        ))
    });
    evidence.dedup_by(|left, right| {
        left.page_index == right.page_index
            && left.reading_order == right.reading_order
            && left.ink_kind == right.ink_kind
            && rect_close(left.source_bounds, right.source_bounds, 0.05)
    });
    Ok(OrphanSourceInk {
        status: "applicable",
        checked_formula_units: checked,
        violations: evidence.len(),
        evidence,
    })
}

fn parse_trace(xml: &str) -> Result<Vec<TracePage>> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut pages = Vec::<TracePage>::new();
    let mut current_page = None::<TracePage>;
    let mut text_transform = None::<[f64; 6]>;
    let mut path_transform = None::<([f64; 6], f64)>;
    let mut path_points = Vec::<Point>::new();
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(tag) if tag.name().as_ref() == b"page" => {
                let media = trace_numbers(&tag, b"mediabox")?;
                current_page = Some(TracePage {
                    height: media.get(3).copied().unwrap_or(0.0)
                        - media.get(1).copied().unwrap_or(0.0),
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
            Event::Start(tag) if tag.name().as_ref() == b"stroke_path" => {
                path_transform = Some((trace_matrix(&tag)?, trace_f64(&tag, b"linewidth")?));
                path_points.clear();
            }
            Event::End(tag) if tag.name().as_ref() == b"stroke_path" => {
                if let (Some(page), Some((_, linewidth)), Some(bounds)) = (
                    current_page.as_mut(),
                    path_transform.take(),
                    points_bounds(&path_points),
                ) {
                    page.ink.push(TraceInk {
                        kind: TraceInkKind::VectorPath,
                        bounds: expand_rect(bounds, linewidth / 2.0),
                    });
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
                            x: device.x,
                            y: page.height - device.y,
                        },
                    });
                }
            }
            Event::Empty(tag)
                if tag.name().as_ref() == b"moveto" || tag.name().as_ref() == b"lineto" =>
            {
                if let (Some(page), Some((transform, _))) = (current_page.as_ref(), path_transform)
                {
                    let device =
                        matrix_point(transform, trace_f64(&tag, b"x")?, trace_f64(&tag, b"y")?);
                    path_points.push(Point {
                        x: device.x,
                        y: page.height - device.y,
                    });
                }
            }
            Event::Empty(tag) if tag.name().as_ref() == b"fill_image" => {
                if let Some(page) = current_page.as_mut() {
                    let transform = trace_matrix(&tag)?;
                    let corners = [
                        matrix_point(transform, 0.0, 0.0),
                        matrix_point(transform, 1.0, 0.0),
                        matrix_point(transform, 0.0, 1.0),
                        matrix_point(transform, 1.0, 1.0),
                    ]
                    .map(|point| Point {
                        x: point.x,
                        y: page.height - point.y,
                    });
                    if let Some(bounds) = points_bounds(&corners) {
                        page.ink.push(TraceInk {
                            kind: TraceInkKind::InlineImage,
                            bounds,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Ok(pages)
}

fn trace_attr(tag: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Result<String> {
    let attribute = tag
        .attributes()
        .find(|attribute| {
            attribute
                .as_ref()
                .is_ok_and(|value| value.key.as_ref() == key)
        })
        .context("trace attribute missing")??;
    Ok(attribute
        .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
        .into_owned())
}

fn trace_f64(tag: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Result<f64> {
    Ok(trace_attr(tag, key)?.parse()?)
}

fn trace_numbers(tag: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Result<Vec<f64>> {
    trace_attr(tag, key)?
        .split_whitespace()
        .map(|value| Ok(value.parse()?))
        .collect()
}

fn trace_matrix(tag: &quick_xml::events::BytesStart<'_>) -> Result<[f64; 6]> {
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

fn rect_close(left: Rect, right: Rect, tolerance: f64) -> bool {
    (left.left - right.left).abs() <= tolerance
        && (left.bottom - right.bottom).abs() <= tolerance
        && (left.right - right.right).abs() <= tolerance
        && (left.top - right.top).abs() <= tolerance
}

fn translated_rect(rect: Rect, delta_x: f64, delta_y: f64) -> Rect {
    Rect {
        left: rect.left + delta_x,
        bottom: rect.bottom + delta_y,
        right: rect.right + delta_x,
        top: rect.top + delta_y,
    }
}

fn point_distance(left: Point, right: Point) -> f64 {
    ((left.x - right.x).powi(2) + (left.y - right.y).powi(2)).sqrt()
}

#[derive(Debug)]
struct FormulaRange {
    start: usize,
    end: usize,
    text: String,
    baseline_y: f64,
    font_size: f64,
    bounds: Option<Rect>,
    metric_bounds: Option<Rect>,
    model_regions: Vec<(usize, Rect)>,
}

fn formula_continuity(
    before: &Il,
    _translated: &Il,
    output_pdf: &Path,
) -> Option<FormulaContinuity> {
    let temp = tempfile::NamedTempFile::new().ok()?;
    let output = Command::new("mutool")
        .args(["draw", "-q", "-F", "stext.json", "-o"])
        .arg(temp.path())
        .arg(output_pdf)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let document: StextDocument = serde_json::from_slice(&fs::read(temp.path()).ok()?).ok()?;
    let units = formula_units(before);
    let mut matched_units = 0;
    let mut measured = 0;
    let mut excessive = 0;
    let mut max_gap = 0.0_f64;
    let mut hole_area = 0.0;
    let mut evidence = Vec::new();
    let mut used = BTreeMap::<usize, Vec<usize>>::new();
    for unit in &units {
        let page = document.pages.get(unit.page_index)?;
        let lines = page
            .blocks
            .iter()
            .flat_map(|block| block.lines.iter().cloned())
            .collect::<Vec<_>>();
        let normalized_formula = compact(&unit.text);
        if normalized_formula.is_empty() {
            continue;
        }
        let used_page = used.entry(unit.page_index).or_default();
        let Some(candidate_indices) = match_formula_lines(
            &lines,
            used_page,
            &normalized_formula,
            unit.expected_stext_y,
            unit.font_size,
        ) else {
            continue;
        };
        used_page.extend(candidate_indices.iter().copied());
        let formula_bbox = candidate_indices
            .iter()
            .map(|index| lines[*index].bbox)
            .reduce(stext_box_union)?;
        matched_units += 1;
        let threshold = continuity_bound(unit.paragraph);
        let (left_gap, right_gap) = formula_neighbor_gaps(&lines, &candidate_indices, formula_bbox);
        let gaps = [
            unit.expects_left_neighbor.then_some(left_gap).flatten(),
            unit.expects_right_neighbor.then_some(right_gap).flatten(),
        ];
        for gap in gaps.into_iter().flatten() {
            measured += 1;
            max_gap = max_gap.max(gap);
            if gap > threshold {
                excessive += 1;
                hole_area += gap * formula_bbox.h.max(1.0);
                evidence.push(GapEvidence {
                    page_index: unit.page_index,
                    reading_order: unit.reading_order,
                    formula: unit.text.clone(),
                    gap_pt: round6(gap),
                    bound_pt: round6(threshold),
                });
            }
        }
    }
    Some(FormulaContinuity {
        status: "applicable",
        formula_units: units.len(),
        matched_units,
        measured_neighbor_gaps: measured,
        excessive_gap_count: excessive,
        max_gap_pt: round6(max_gap),
        unexplained_hole_count: excessive,
        unexplained_hole_area_pt2: round6(hole_area),
        evidence,
    })
}

fn formula_neighbor_gaps(
    lines: &[StextLine],
    formula_indices: &[usize],
    formula_bbox: StextBox,
) -> (Option<f64>, Option<f64>) {
    let mut left_gap: Option<f64> = None;
    let mut right_gap: Option<f64> = None;
    for (index, neighbor) in lines.iter().enumerate() {
        if formula_indices.contains(&index)
            || !mimus_quality_contract::formula_items_share_line(
                formula_bbox.y,
                formula_bbox.y + formula_bbox.h,
                neighbor.bbox.y,
                neighbor.bbox.y + neighbor.bbox.h,
            )
        {
            continue;
        }
        let formula_left = formula_bbox.x;
        let formula_right = formula_bbox.x + formula_bbox.w;
        let neighbor_left = neighbor.bbox.x;
        let neighbor_right = neighbor.bbox.x + neighbor.bbox.w;
        if neighbor_right <= formula_left {
            let gap = formula_left - neighbor_right;
            left_gap = Some(left_gap.map_or(gap, |current| current.min(gap)));
        } else if neighbor_left >= formula_right {
            let gap = neighbor_left - formula_right;
            right_gap = Some(right_gap.map_or(gap, |current| current.min(gap)));
        }
    }
    (left_gap, right_gap)
}

fn match_formula_lines(
    lines: &[StextLine],
    used: &[usize],
    formula: &str,
    expected_y: f64,
    font_size: f64,
) -> Option<Vec<usize>> {
    let mut candidates = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| {
            let line_center = line.bbox.y + line.bbox.h / 2.0;
            !used.contains(index)
                && !compact(&line.text).is_empty()
                && (line_center - expected_y).abs() <= font_size * 1.5
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(_, left), (_, right)| left.bbox.x.total_cmp(&right.bbox.x));

    for start in 0..candidates.len() {
        let mut matched = String::new();
        let mut indices = Vec::new();
        for (index, line) in &candidates[start..] {
            matched.push_str(&compact(&line.text));
            if !formula.starts_with(&matched) {
                break;
            }
            indices.push(*index);
            if matched == formula {
                return Some(indices);
            }
        }
    }
    None
}

fn stext_box_union(left: StextBox, right: StextBox) -> StextBox {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    let right_edge = (left.x + left.w).max(right.x + right.w);
    let bottom_edge = (left.y + left.h).max(right.y + right.h);
    StextBox {
        x,
        y,
        w: right_edge - x,
        h: bottom_edge - y,
    }
}

fn formula_units(il: &Il) -> Vec<FormulaUnit<'_>> {
    let mut out = Vec::new();
    for page in &il.pages {
        for paragraph in &page.paragraphs {
            let mut ranges = Vec::<FormulaRange>::new();
            let mut current = None::<FormulaRange>;
            for (index, c) in paragraph.text.chars.iter().enumerate() {
                if is_inline_formula(c) {
                    let range = current.get_or_insert_with(|| FormulaRange {
                        start: index,
                        end: index,
                        text: String::new(),
                        baseline_y: c.baseline_origin.y,
                        font_size: 0.0,
                        bounds: None,
                        metric_bounds: None,
                        model_regions: Vec::new(),
                    });
                    range.end = index;
                    range.font_size = range.font_size.max(c.font_size);
                    range.bounds = match (range.bounds, char_bounds(std::slice::from_ref(c))) {
                        (Some(left), Some(right)) => Some(rect_union(left, right)),
                        (None, bounds) | (bounds, None) => bounds,
                    };
                    range.metric_bounds = Some(
                        range
                            .metric_bounds
                            .map_or(c.box_, |bounds| rect_union(bounds, c.box_)),
                    );
                    range.text.push_str(c.unicode.as_deref().unwrap_or(""));
                    if let Some(layout) = c.layout.as_ref().filter(|layout| {
                        layout.source == "model" && layout.label == "inline_formula"
                    }) {
                        let region = (layout.reading_order, layout.bounds);
                        if !range.model_regions.contains(&region) {
                            range.model_regions.push(region);
                        }
                    }
                } else if let Some(range) = current.take() {
                    ranges.push(range);
                }
            }
            if let Some(range) = current {
                ranges.push(range);
            }

            let continuity_limit = continuity_bound(paragraph);
            let mut range_index = 0;
            while range_index < ranges.len() {
                let mut range = FormulaRange {
                    start: ranges[range_index].start,
                    end: ranges[range_index].end,
                    text: ranges[range_index].text.clone(),
                    baseline_y: ranges[range_index].baseline_y,
                    font_size: ranges[range_index].font_size,
                    bounds: ranges[range_index].bounds,
                    metric_bounds: ranges[range_index].metric_bounds,
                    model_regions: ranges[range_index].model_regions.clone(),
                };
                let mut right_neighbor_override = false;
                if let Some(following) = ranges.get(range_index + 1) {
                    let between = &paragraph.text.chars[range.end + 1..following.start];
                    let following_end = ranges
                        .get(range_index + 2)
                        .map_or(paragraph.text.chars.len(), |next| next.start);
                    let after_following = &paragraph.text.chars[following.end + 1..following_end];
                    if scorecard_interleaved_formula_segment(
                        &range,
                        between,
                        following,
                        after_following,
                        continuity_limit,
                    ) {
                        range.end = following.end;
                        range.text.push_str(&following.text);
                        range.font_size = range.font_size.max(following.font_size);
                        range.bounds = match (range.bounds, following.bounds) {
                            (Some(left), Some(right)) => Some(rect_union(left, right)),
                            (None, bounds) | (bounds, None) => bounds,
                        };
                        range.metric_bounds = match (range.metric_bounds, following.metric_bounds) {
                            (Some(left), Some(right)) => Some(rect_union(left, right)),
                            (None, bounds) | (bounds, None) => bounds,
                        };
                        for region in &following.model_regions {
                            if !range.model_regions.contains(region) {
                                range.model_regions.push(*region);
                            }
                        }
                        right_neighbor_override = between.iter().any(qualifies_formula_neighbor);
                        range_index += 1;
                    }
                }
                let (left, right) = expected_formula_neighbors(
                    &paragraph.text.chars,
                    range.start,
                    range.end,
                    range.baseline_y,
                    range.font_size,
                );
                let attached_radical =
                    uniquely_attached_scorecard_radical(paragraph, &ranges, range.bounds);
                let mut text = range.text;
                let mut source_bounds = range.bounds;
                let mut source_glyphs = Vec::new();
                if let Some(radical) = attached_radical {
                    text.insert(0, '\u{221a}');
                    source_bounds =
                        match (source_bounds, char_bounds(std::slice::from_ref(radical))) {
                            (Some(left), Some(right)) => Some(rect_union(left, right)),
                            (None, bounds) | (bounds, None) => bounds,
                        };
                    source_glyphs.push(('\u{221a}', radical.baseline_origin));
                }
                source_glyphs.extend(
                    paragraph.text.chars[range.start..=range.end]
                        .iter()
                        .filter(|character| is_inline_formula(character))
                        .filter_map(|character| {
                            character
                                .unicode
                                .as_deref()
                                .and_then(|value| value.chars().next())
                                .map(|value| (value, character.baseline_origin))
                        }),
                );
                out.push(FormulaUnit {
                    page_index: page.index,
                    reading_order: paragraph.reading_order,
                    paragraph,
                    text,
                    expected_stext_y: page.geometry.height - range.baseline_y,
                    font_size: range.font_size,
                    expects_left_neighbor: left,
                    expects_right_neighbor: right || right_neighbor_override,
                    source_bounds,
                    source_glyphs,
                    has_attached_source_radical: attached_radical.is_some(),
                });
                range_index += 1;
            }
        }
    }
    out
}

fn uniquely_attached_scorecard_radical<'a>(
    paragraph: &'a Paragraph,
    ranges: &[FormulaRange],
    formula_bounds: Option<Rect>,
) -> Option<&'a Char> {
    let formula_bounds = formula_bounds?;
    let matching = paragraph
        .text
        .chars
        .iter()
        .filter(|character| {
            !is_inline_formula(character)
                && character.unicode.as_deref() == Some("\u{221a}")
                && scorecard_radical_attaches_to_formula(character, formula_bounds)
                && ranges
                    .iter()
                    .filter_map(|range| range.bounds)
                    .filter(|bounds| scorecard_radical_attaches_to_formula(character, *bounds))
                    .count()
                    == 1
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [radical] => Some(*radical),
        _ => None,
    }
}

fn scorecard_radical_attaches_to_formula(radical: &Char, formula_bounds: Rect) -> bool {
    let Some(bounds) = char_bounds(std::slice::from_ref(radical)) else {
        return false;
    };
    let em = radical.font_size.max(0.01);
    let gap = formula_bounds.left - bounds.right;
    let overlaps_vertically =
        bounds.top > formula_bounds.bottom + 0.01 && formula_bounds.top > bounds.bottom + 0.01;
    overlaps_vertically && gap >= -0.05 * em && gap <= 0.25 * em
}

fn scorecard_interleaved_formula_segment(
    preceding: &FormulaRange,
    between: &[Char],
    following: &FormulaRange,
    after_following: &[Char],
    limit: f64,
) -> bool {
    if between.is_empty()
        || !limit.is_finite()
        || limit <= 0.0
        || !between.iter().any(|character| {
            character
                .unicode
                .as_deref()
                .is_some_and(|value| value.chars().any(|value| !value.is_whitespace()))
        })
    {
        return false;
    }
    let Some(punctuation_bounds) = char_bounds(between) else {
        return false;
    };
    let Some(formula_bounds) = following.bounds else {
        return false;
    };
    let punctuation_only = between.iter().all(|character| {
        character.unicode.as_deref().is_some_and(|value| {
            value
                .chars()
                .all(|value| value.is_whitespace() || is_punctuation(value))
        })
    });
    let short_extraction_inversion = punctuation_only && after_following.is_empty();
    let complete_formula_inversion = between
        .iter()
        .filter_map(|character| character.unicode.as_deref())
        .flat_map(str::chars)
        .find(|value| !value.is_whitespace())
        .is_some_and(is_punctuation)
        && formula_ranges_share_model_region(preceding, following)
        && preceding
            .metric_bounds
            .zip(following.metric_bounds)
            .is_some_and(|(preceding_bounds, following_bounds)| {
                formula_rects_are_adjacent(preceding_bounds, following_bounds, limit)
            })
        && between.iter().all(|character| {
            char_bounds(std::slice::from_ref(character))
                .is_some_and(|bounds| rects_overlap_vertically(bounds, formula_bounds))
        });
    formula_rects_are_adjacent(formula_bounds, punctuation_bounds, limit)
        && (short_extraction_inversion || complete_formula_inversion)
}

fn formula_ranges_share_model_region(left: &FormulaRange, right: &FormulaRange) -> bool {
    left.model_regions
        .iter()
        .any(|region| right.model_regions.contains(region))
}

fn char_bounds(chars: &[Char]) -> Option<Rect> {
    chars
        .iter()
        .flat_map(|character| [Some(character.box_), character.visual_bbox])
        .flatten()
        .filter(|bounds| {
            bounds.left.is_finite()
                && bounds.bottom.is_finite()
                && bounds.right.is_finite()
                && bounds.top.is_finite()
        })
        .reduce(rect_union)
}

fn rect_union(left: Rect, right: Rect) -> Rect {
    Rect {
        left: left.left.min(right.left),
        bottom: left.bottom.min(right.bottom),
        right: left.right.max(right.right),
        top: left.top.max(right.top),
    }
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

fn expected_formula_neighbors(
    chars: &[Char],
    start: usize,
    end: usize,
    baseline_y: f64,
    font_size: f64,
) -> (bool, bool) {
    let qualifies = |c: &Char| {
        qualifies_formula_neighbor(c) && (c.baseline_origin.y - baseline_y).abs() <= font_size * 0.3
    };
    (
        start
            .checked_sub(1)
            .is_some_and(|index| qualifies(&chars[index])),
        chars.get(end + 1).is_some_and(qualifies),
    )
}

fn qualifies_formula_neighbor(character: &Char) -> bool {
    character
        .layout
        .as_ref()
        .is_some_and(|layout| layout.policy == "translate")
}

fn continuity_bound(paragraph: &Paragraph) -> f64 {
    let translates = |character: &Char| {
        character
            .layout
            .as_ref()
            .is_some_and(|layout| layout.policy == "translate")
    };
    let mut word_gaps = paragraph
        .text
        .chars
        .iter()
        .filter(|character| {
            character
                .unicode
                .as_deref()
                .is_some_and(|value| !value.is_empty() && value.chars().all(char::is_whitespace))
        })
        .map(|character| character.box_.right - character.box_.left)
        .filter(|gap| gap.is_finite() && *gap > 0.0)
        .collect::<Vec<_>>();
    word_gaps.extend(paragraph.text.chars.windows(2).filter_map(|pair| {
        let [left, right] = pair else {
            return None;
        };
        if !right.implicit_space_before
            || !translates(left)
            || !translates(right)
            || (left.baseline_origin.y - right.baseline_origin.y).abs()
                > left.font_size.max(right.font_size) * 0.35
        {
            return None;
        }
        let gap = right.box_.left - left.box_.right;
        (gap.is_finite() && gap > 0.0).then_some(gap)
    }));
    let font_sizes = paragraph
        .text
        .chars
        .iter()
        .map(|character| character.font_size)
        .filter(|size| size.is_finite() && *size > 0.0)
        .collect::<Vec<_>>();
    mimus_quality_contract::formula_continuity_limit(word_gaps, font_sizes).unwrap_or(0.0)
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

#[derive(Debug, Serialize)]
struct TitleAuthorConservation {
    status: &'static str,
    title_paragraphs: usize,
    author_paragraphs: usize,
    title_policy_passthrough: bool,
    author_policy_passthrough: bool,
    title_write_identity: bool,
    author_write_identity: bool,
    band_lower: f64,
    band_upper: f64,
    band_tolerance: f64,
    source_hash_sha256: String,
    write_hash_sha256: Option<String>,
    failures: usize,
}

fn title_author_conservation(before: &Il, write: &Il) -> Option<TitleAuthorConservation> {
    let page = before.pages.iter().find(|page| page.index == 0)?;
    let title = page
        .paragraphs
        .iter()
        .find(|paragraph| has_only_label(paragraph, "doc_title"))?;
    let lower = page
        .paragraphs
        .iter()
        .filter(|paragraph| {
            has_only_label(paragraph, "abstract") || has_only_label(paragraph, "paragraph_title")
        })
        .filter(|paragraph| paragraph.bounds.top < title.bounds.bottom)
        .max_by(|left, right| left.bounds.top.total_cmp(&right.bounds.top))?;
    let band = title_author_band(
        title.bounds.bottom,
        lower.bounds.top,
        title
            .text
            .chars
            .iter()
            .chain(&lower.text.chars)
            .map(|character| character.font_size),
    )?;
    let authors = page
        .paragraphs
        .iter()
        .filter(|paragraph| {
            (has_only_label(paragraph, "text") || has_only_label(paragraph, "fallback_line"))
                && band.contains(paragraph.bounds.bottom, paragraph.bounds.top)
        })
        .collect::<Vec<_>>();
    let title_after = find_paragraph(write, 0, title.reading_order);
    let title_policy = paragraph_passthrough(title);
    let author_policy = !authors.is_empty()
        && authors
            .iter()
            .all(|paragraph| paragraph_passthrough(paragraph));
    let title_identity = title_after.is_some_and(|after| paragraph_identity(title, after));
    let author_identity = !authors.is_empty()
        && authors.iter().all(|source| {
            find_paragraph(write, 0, source.reading_order)
                .is_some_and(|after| paragraph_identity(source, after))
        });
    let selected = std::iter::once(title)
        .chain(authors.iter().copied())
        .collect::<Vec<_>>();
    let write_selected = selected
        .iter()
        .map(|paragraph| find_paragraph(write, 0, paragraph.reading_order))
        .collect::<Option<Vec<_>>>();
    let failures = [title_policy, author_policy, title_identity, author_identity]
        .iter()
        .filter(|value| !**value)
        .count();
    Some(TitleAuthorConservation {
        status: "applicable",
        title_paragraphs: 1,
        author_paragraphs: authors.len(),
        title_policy_passthrough: title_policy,
        author_policy_passthrough: author_policy,
        title_write_identity: title_identity,
        author_write_identity: author_identity,
        band_lower: band.lower,
        band_upper: band.upper,
        band_tolerance: band.tolerance,
        source_hash_sha256: title_author_hash(&selected),
        write_hash_sha256: write_selected.as_deref().map(title_author_hash),
        failures,
    })
}

fn title_author_hash(paragraphs: &[&Paragraph]) -> String {
    let canonical = paragraphs
        .iter()
        .map(|paragraph| {
            let chars = paragraph
                .text
                .chars
                .iter()
                .map(|character| {
                    serde_json::json!({
                        "unicode": character.unicode,
                        "code": character.code,
                        "font": character.font,
                        "font_size": character.font_size,
                        "baseline_origin": character.baseline_origin,
                        "box": character.box_,
                        "visual_bbox": character.visual_bbox,
                        "layout_label": character.layout.as_ref().map(|layout| layout.label.as_str()),
                        "layout_policy": character.layout.as_ref().map(|layout| layout.policy.as_str()),
                        "passthrough": character.passthrough,
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "page_index": 0,
                "reading_order": paragraph.reading_order,
                "bounds": paragraph.bounds,
                "translated_text": paragraph.translated_text,
                "chars": chars,
            })
        })
        .collect::<Vec<_>>();
    sha256(&serde_json::to_vec(&canonical).unwrap())
}

fn has_only_label(paragraph: &Paragraph, label: &str) -> bool {
    !paragraph.text.chars.is_empty()
        && paragraph.text.chars.iter().all(|character| {
            character
                .layout
                .as_ref()
                .is_some_and(|layout| layout.label == label)
        })
}

fn paragraph_passthrough(paragraph: &Paragraph) -> bool {
    !paragraph.text.chars.is_empty()
        && paragraph.text.chars.iter().all(|c| {
            c.layout
                .as_ref()
                .is_some_and(|layout| layout.policy == "passthrough")
        })
}

fn paragraph_identity(before: &Paragraph, after: &Paragraph) -> bool {
    before.text.chars.len() == after.text.chars.len()
        && before
            .text
            .chars
            .iter()
            .zip(&after.text.chars)
            .all(|(a, b)| {
                a.unicode == b.unicode
                    && a.code == b.code
                    && a.font_size == b.font_size
                    && a.baseline_origin.x == b.baseline_origin.x
                    && a.baseline_origin.y == b.baseline_origin.y
                    && a.box_.left == b.box_.left
                    && a.box_.bottom == b.box_.bottom
                    && a.box_.right == b.box_.right
                    && a.box_.top == b.box_.top
                    && a.visual_bbox == b.visual_bbox
                    && a.font == b.font
                    && a.passthrough == b.passthrough
            })
        && after
            .translated_text
            .as_deref()
            .is_none_or(|text| text_equivalent(text, &source_text(before)))
}

fn applicability_value(applicable: bool, reason: &'static str) -> Applicability {
    Applicability {
        status: if applicable {
            "applicable"
        } else {
            "not-applicable"
        },
        reason: (!applicable).then_some(reason),
    }
}

fn not_applicable(reason: &'static str) -> Value {
    serde_json::json!({"status": "not-applicable", "reason": reason})
}

fn read_glossary(path: &Path) -> Result<Glossary> {
    let parsed: Glossary = toml::from_str(
        &fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    if parsed.version != 1 {
        bail!("unsupported glossary version {}", parsed.version)
    }
    Ok(parsed)
}

fn process_metrics(
    before: &Il,
    events: &[Value],
    process_log: Option<&Path>,
    resource_usage: Option<&Path>,
) -> Result<ProcessMetrics> {
    let eligible_paragraphs = before
        .pages
        .iter()
        .flat_map(|page| &page.paragraphs)
        .filter(|paragraph| is_translatable(paragraph))
        .count();
    let calls = process_log.map(read_ndjson).transpose()?;
    let translation_calls = calls.as_ref().map(|values| {
        values
            .iter()
            .filter(|value| value.get("term_extraction").and_then(Value::as_bool) == Some(false))
            .count()
    });
    let term_extraction_calls = calls.as_ref().map(|values| {
        values
            .iter()
            .filter(|value| value.get("term_extraction").and_then(Value::as_bool) == Some(true))
            .count()
    });
    let resource = resource_usage
        .map(fs::read_to_string)
        .transpose()?
        .unwrap_or_default();
    let retry_diagnostics = count_diagnostic_matching(events, |id| id.contains("retry"));
    let suspicious_echoes = count_summary_array(events, "suspicious_echoes");
    let cache_hits = count_cache_events(events, "hit");
    let cache_misses = count_cache_events(events, "miss");
    Ok(ProcessMetrics {
        formula_ids: vec!["PRO-01", "PRO-02", "PRO-03", "PRO-04", "PRO-05"],
        terminal_result: events
            .iter()
            .any(|value| value.get("event").and_then(Value::as_str) == Some("result")),
        internal_errors: events
            .iter()
            .filter(|value| {
                value.get("event").and_then(Value::as_str) == Some("error")
                    && value.get("category").and_then(Value::as_str) == Some("internal")
            })
            .count(),
        eligible_paragraphs,
        typed_degraded_paragraphs: events
            .iter()
            .find(|value| value.get("id").and_then(Value::as_str) == Some("degradation_summary"))
            .and_then(|value| value.get("preserved_paragraph_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        translation_calls,
        translation_calls_per_eligible_paragraph: translation_calls
            .and_then(|calls| optional_rate(calls, eligible_paragraphs)),
        term_extraction_calls,
        retry_diagnostics,
        retry_rate: translation_calls.and_then(|calls| optional_rate(retry_diagnostics, calls)),
        suspicious_echoes,
        echo_rate: optional_rate(suspicious_echoes, eligible_paragraphs).unwrap_or(0.0),
        cache_hits,
        cache_misses,
        cache_hit_rate: optional_rate(cache_hits, cache_hits + cache_misses),
        wall_time_seconds: parse_resource_f64(&resource, "real"),
        peak_rss_bytes: parse_resource_u64(&resource, "maximum resident set size"),
        per_page_timing: None,
    })
}

fn count_cache_events(events: &[Value], status: &str) -> usize {
    events
        .iter()
        .filter(|value| {
            value.get("event").and_then(Value::as_str) == Some("translation_cache")
                && value.get("status").and_then(Value::as_str) == Some(status)
        })
        .count()
}

fn optional_rate(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| round6(numerator as f64 / denominator as f64))
}

fn count_diagnostic_matching(events: &[Value], predicate: impl Fn(&str) -> bool) -> usize {
    events
        .iter()
        .filter(|value| {
            value.get("event").and_then(Value::as_str) == Some("diagnostic")
                && value
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(&predicate)
        })
        .count()
}

fn parse_resource_f64(text: &str, key: &str) -> Option<f64> {
    text.lines().find_map(|line| {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        fields.windows(2).find_map(|pair| {
            if pair[0] == key {
                pair[1].parse().ok()
            } else if pair[1] == key {
                pair[0].parse().ok()
            } else {
                None
            }
        })
    })
}

fn parse_resource_u64(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let line = line.trim();
        line.strip_suffix(key)
            .and_then(|value| value.trim().parse().ok())
            .or_else(|| {
                line.strip_prefix(key)
                    .and_then(|value| value.trim().parse().ok())
            })
    })
}

fn paragraph_pairs<'a>(a: &'a Il, b: &'a Il) -> Vec<(&'a Paragraph, &'a Paragraph)> {
    let after = b
        .pages
        .iter()
        .flat_map(|page| {
            page.paragraphs
                .iter()
                .map(move |paragraph| ((page.index, paragraph.reading_order), paragraph))
        })
        .collect::<BTreeMap<_, _>>();
    a.pages
        .iter()
        .flat_map(|page| {
            page.paragraphs.iter().filter_map(|paragraph| {
                after
                    .get(&(page.index, paragraph.reading_order))
                    .map(|other| (paragraph, *other))
            })
        })
        .collect()
}
fn source_text(p: &Paragraph) -> String {
    p.text
        .chars
        .iter()
        .filter_map(|c| c.unicode.as_deref())
        .collect()
}
#[cfg(test)]
fn translate_source_text(p: &Paragraph) -> String {
    translate_source_text_for_page(p, None)
}
fn translate_source_text_for_page(
    p: &Paragraph,
    direct_content_objects: Option<&BTreeSet<u32>>,
) -> String {
    let mut output = String::new();
    for current in &p.text.chars {
        if !character_is_translation_source(current, direct_content_objects) {
            continue;
        }
        let Some(unicode) = current.unicode.as_deref() else {
            continue;
        };
        if current.implicit_space_before
            && !output.ends_with(char::is_whitespace)
            && !unicode.starts_with(char::is_whitespace)
        {
            output.push(' ');
        }
        output.push_str(unicode);
    }
    output
}
fn character_is_translation_source(
    character: &Char,
    direct_content_objects: Option<&BTreeSet<u32>>,
) -> bool {
    character.visible
        && character
            .text_transform
            .as_ref()
            .is_none_or(|transform| transform.kind == "upright")
        && character
            .layout
            .as_ref()
            .is_some_and(|layout| layout.policy == "translate")
        && direct_content_objects.is_none_or(|objects| {
            character
                .passthrough
                .as_ref()
                .and_then(|passthrough| passthrough.get("content_object"))
                .and_then(Value::as_u64)
                .and_then(|object| u32::try_from(object).ok())
                .is_some_and(|object| objects.contains(&object))
        })
}
fn find_paragraph(il: &Il, page_index: usize, reading_order: usize) -> Option<&Paragraph> {
    il.pages
        .iter()
        .find(|page| page.index == page_index)?
        .paragraphs
        .iter()
        .find(|paragraph| paragraph.reading_order == reading_order)
}
fn count_han(text: &str) -> usize {
    text.chars().filter(|c| is_han(*c)).count()
}
fn is_han(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x3134f
    )
}
fn text_equivalent(a: &str, b: &str) -> bool {
    a.chars()
        .filter(|c| !c.is_whitespace())
        .eq(b.chars().filter(|c| !c.is_whitespace()))
}
fn is_translatable(p: &Paragraph) -> bool {
    p.text
        .chars
        .iter()
        .filter_map(|c| c.layout.as_ref())
        .any(|l| l.policy == "translate")
}
fn preserved_reason(p: &Paragraph) -> Option<String> {
    p.preserved
        .as_ref()
        .and_then(|v| v.get("reason").or(Some(v)))
        .and_then(Value::as_str)
        .map(str::to_owned)
}
fn looks_like_superscript(p: &Paragraph) -> bool {
    if p.text.chars.len() < 2 {
        return false;
    }
    let mut sizes: Vec<f64> = p.text.chars.iter().map(|c| c.font_size).collect();
    let bases: Vec<f64> = p.text.chars.iter().map(|c| c.baseline_origin.y).collect();
    let median_size = median(&mut sizes);
    let mut sorted_bases = bases.clone();
    let median_base = median(&mut sorted_bases);
    p.text.chars.iter().any(|c| {
        c.font_size <= median_size * 0.8
            && (c.baseline_origin.y - median_base).abs() >= median_size * 0.15
    })
}
fn is_numbering(t: &str) -> bool {
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || ".()[]- ".contains(c))
}
fn median_font(p: &Paragraph) -> f64 {
    let mut v: Vec<f64> = p.text.chars.iter().map(|c| c.font_size).collect();
    median(&mut v)
}
fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n.is_multiple_of(2) {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    } else {
        v[n / 2]
    }
}
fn iou(a: Rect, b: Rect) -> f64 {
    let iw = (a.right.min(b.right) - a.left.max(b.left)).max(0.0);
    let ih = (a.top.min(b.top) - a.bottom.max(b.bottom)).max(0.0);
    let i = iw * ih;
    let u = (a.right - a.left).max(0.0) * (a.top - a.bottom).max(0.0)
        + (b.right - b.left).max(0.0) * (b.top - b.bottom).max(0.0)
        - i;
    ratio(i, u)
}
fn ratio(a: f64, b: f64) -> f64 {
    if b == 0.0 { 1.0 } else { round6(a / b) }
}
fn round6(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation() || "，。！？；：、（）【】《》“”‘’".contains(c)
}
fn values(items: &[(&str, usize)]) -> BTreeMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), (*v).into()))
        .collect()
}

fn default_true() -> bool {
    true
}

fn count_diagnostic(events: &[Value], id: &str) -> usize {
    events
        .iter()
        .filter(|v| {
            v.get("event").and_then(Value::as_str) == Some("diagnostic")
                && v.get("id").and_then(Value::as_str) == Some(id)
        })
        .count()
}
fn count_summary_array(events: &[Value], key: &str) -> usize {
    events
        .iter()
        .find_map(|v| v.get(key).and_then(Value::as_array).map(Vec::len))
        .unwrap_or(0)
}
fn read_ndjson(path: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(Into::into))
        .collect()
}
fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T> {
    serde_json::from_slice(&fs::read(&path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?
    }
    let mut f = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    f.write_all(bytes)?;
    f.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn qpdf_check(path: &Path) -> bool {
    Command::new("qpdf")
        .arg("--check")
        .arg(path)
        .output()
        .is_ok_and(|o| o.status.success())
}
fn extracted_character_count(path: &Path) -> Result<usize> {
    let output = Command::new("pdftotext").arg(path).arg("-").output()?;
    if !output.status.success() {
        bail!(
            "pdftotext failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .chars()
        .filter(|c| !c.is_whitespace())
        .count())
}
fn qpdf_pages(path: &Path) -> Option<usize> {
    let o = Command::new("qpdf")
        .arg("--show-npages")
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8(o.stdout).ok()?.trim().parse().ok()
}
fn qpdf_non_text_counts(path: &Path) -> Result<BTreeMap<String, usize>> {
    let o = Command::new("qpdf").arg("--json").arg(path).output()?;
    if !o.status.success() {
        bail!("qpdf --json failed for {}", path.display())
    }
    let v: Value = serde_json::from_slice(&o.stdout)?;
    let mut out = BTreeMap::new();
    count_json_names(&v, &mut out);
    Ok(out)
}
fn qpdf_direct_content_objects(path: &Path) -> Result<BTreeMap<usize, BTreeSet<u32>>> {
    let output = Command::new("qpdf")
        .args(["--json-key=pages", "--json"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        bail!(
            "qpdf page JSON failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    direct_content_objects_from_qpdf_json(&value)
}
fn direct_content_objects_from_qpdf_json(value: &Value) -> Result<BTreeMap<usize, BTreeSet<u32>>> {
    let pages = value
        .get("pages")
        .and_then(Value::as_array)
        .context("qpdf page JSON has no pages array")?;
    pages
        .iter()
        .enumerate()
        .map(|(page_index, page)| {
            let contents = page
                .get("contents")
                .and_then(Value::as_array)
                .context("qpdf page JSON has no contents array")?;
            let objects = contents
                .iter()
                .map(|reference| {
                    let reference = reference
                        .as_str()
                        .context("qpdf content reference is not a string")?;
                    let mut fields = reference.split_ascii_whitespace();
                    let object = fields
                        .next()
                        .context("qpdf content reference has no object number")?
                        .parse::<u32>()?;
                    let generation = fields
                        .next()
                        .context("qpdf content reference has no generation number")?;
                    let marker = fields
                        .next()
                        .context("qpdf content reference has no reference marker")?;
                    if generation.parse::<u16>().is_err()
                        || marker != "R"
                        || fields.next().is_some()
                    {
                        bail!("invalid qpdf content reference {reference:?}")
                    }
                    Ok(object)
                })
                .collect::<Result<BTreeSet<_>>>()?;
            Ok((page_index, objects))
        })
        .collect()
}
fn count_json_names(v: &Value, out: &mut BTreeMap<String, usize>) {
    match v {
        Value::String(s)
            if ["/Form", "/Image", "/Link", "/Annot", "/Outlines"].contains(&s.as_str()) =>
        {
            *out.entry(s.clone()).or_default() += 1
        }
        Value::Array(a) => a.iter().for_each(|v| count_json_names(v, out)),
        Value::Object(m) => m.values().for_each(|v| count_json_names(v, out)),
        _ => {}
    }
}

fn translated_masks(il: &Il) -> BTreeMap<usize, Vec<Rect>> {
    il.pages
        .iter()
        .map(|p| {
            (
                p.index,
                p.paragraphs
                    .iter()
                    .filter(|x| x.translated_text.is_some() && x.preserved.is_none())
                    .map(|x| x.bounds)
                    .collect(),
            )
        })
        .collect()
}
struct Pix {
    w: usize,
    h: usize,
    data: Vec<u8>,
}
fn pixel_fidelity(
    input: &Path,
    output: &Path,
    masks: &BTreeMap<usize, Vec<Rect>>,
    dpi: u32,
) -> Result<f64> {
    let dir = tempfile::tempdir()?;
    let a = dir.path().join("in");
    let b = dir.path().join("out");
    render(input, &a, dpi)?;
    render(output, &b, dpi)?;
    let input_pages = rendered_pages(dir.path(), "in-")?;
    let output_pages = rendered_pages(dir.path(), "out-")?;
    if input_pages.len() != output_pages.len() {
        bail!("rendered page counts differ")
    }
    let mut same = 0u64;
    let mut total = 0u64;
    for (page_index, (ap, bp)) in input_pages.iter().zip(&output_pages).enumerate() {
        let x = parse_ppm(ap)?;
        let y = parse_ppm(bp)?;
        if x.w != y.w || x.h != y.h {
            bail!("render dimensions differ on page {}", page_index + 1)
        }
        let scale = dpi as f64 / 72.0;
        for py in 0..x.h {
            for px in 0..x.w {
                let pdf_x = px as f64 / scale;
                let pdf_y = (x.h - py) as f64 / scale;
                if masks.get(&page_index).is_some_and(|rs| {
                    rs.iter().any(|r| {
                        pdf_x >= r.left - 2.0
                            && pdf_x <= r.right + 2.0
                            && pdf_y >= r.bottom - 2.0
                            && pdf_y <= r.top + 2.0
                    })
                }) {
                    continue;
                }
                let i = (py * x.w + px) * 3;
                total += 1;
                if x.data[i..i + 3] == y.data[i..i + 3] {
                    same += 1
                }
            }
        }
    }
    Ok(ratio(same as f64, total as f64))
}
fn rendered_pages(dir: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".ppm"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}
fn render(pdf: &Path, prefix: &Path, dpi: u32) -> Result<()> {
    let o = Command::new("pdftoppm")
        .args(["-r", &dpi.to_string(), "-aa", "no", "-aaVector", "no"])
        .arg(pdf)
        .arg(prefix)
        .output()?;
    if !o.status.success() {
        bail!("pdftoppm failed: {}", String::from_utf8_lossy(&o.stderr))
    }
    Ok(())
}
fn parse_ppm(path: &Path) -> Result<Pix> {
    let bytes = fs::read(path)?;
    let mut ends = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            ends.push(i);
            if ends.len() == 3 {
                break;
            }
        }
    }
    if ends.len() < 3 || &bytes[..2] != b"P6" {
        bail!("unsupported PPM {}", path.display())
    }
    let dims = std::str::from_utf8(&bytes[ends[0] + 1..ends[1]])?;
    let mut it = dims.split_whitespace();
    let w = it.next().context("PPM width")?.parse()?;
    let h = it.next().context("PPM height")?.parse()?;
    if &bytes[ends[1] + 1..ends[2]] != b"255" {
        bail!("unsupported PPM depth")
    }
    Ok(Pix {
        w,
        h,
        data: bytes[ends[2] + 1..].to_vec(),
    })
}

fn markdown(r: &Report) -> String {
    let mut s = String::from(
        "# Quality scorecard\n\n| Dimension | Score | Weighted errors / 1k chars |\n| --- | ---: | ---: |\n",
    );
    for (k, v) in &r.dimensions {
        s.push_str(&format!(
            "| {k} | {:.3} | {:.3} |\n",
            v.score, v.errors_per_1000_output_characters
        ));
    }
    s.push_str(&format!(
        "| **total** | **{:.3}** | |\n\nConclusion: `{}`. Output characters: {}. Schema: v{}.\n",
        r.total_score, r.conclusion.status, r.output_characters, r.schema_version
    ));
    if !r.conclusion.confirmed_criticals.is_empty() {
        s.push_str("\nHuman-confirmed critical defects (override the automatic total):\n\n");
        for defect in &r.conclusion.confirmed_criticals {
            s.push_str(&format!("- {defect}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn il(policy: &str, translated_text: Option<&str>, preserved: Option<Value>) -> Il {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "pages": [{
                "index": 0,
                "geometry": {"width": 100.0, "height": 100.0},
                "paragraphs": [{
                    "reading_order": 7,
                    "bounds": {"left": 1.0, "bottom": 2.0, "right": 9.0, "top": 8.0},
                    "text": {"kind": "chars", "chars": [{
                        "unicode": "A",
                        "font_size": 10.0,
                        "baseline_origin": {"x": 1.0, "y": 2.0},
                        "layout": {"label": "text", "policy": policy}
                    }]},
                    "translated_text": translated_text,
                    "preserved": preserved
                }]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn hand_built_artifact_reports_typed_coverage_gap() {
        let before = il("translate", None, None);
        let after = il(
            "translate",
            Some("A"),
            Some(Value::String("unreliable_unicode".into())),
        );
        let result = coverage(&before, &after, &after, 1000.0);
        assert_eq!(result.weighted_errors, 5.0);
        assert_eq!(result.measurements["paragraph_coverage"], 0.0);
        assert_eq!(
            result.measurements["preserved_reasons"]["unreliable_unicode"],
            1
        );
    }

    #[test]
    fn form_xobject_preservation_is_reported_in_the_coverage_reason_distribution() {
        let before = il("translate", None, None);
        let after = il(
            "translate",
            None,
            Some(Value::String("form_xobject_content".into())),
        );

        let result = coverage(&before, &after, &after, 1000.0);

        assert_eq!(result.measurements["eligible_paragraphs"], 1);
        assert_eq!(result.measurements["translated_paragraphs"], 0);
        assert_eq!(result.measurements["paragraph_coverage"], 0.0);
        assert_eq!(
            result.measurements["preserved_reasons"]["form_xobject_content"],
            1
        );
    }

    #[test]
    fn cache_metrics_count_protocol_events() {
        let events = vec![
            serde_json::json!({"event":"translation_cache", "status":"hit"}),
            serde_json::json!({"event":"translation_cache", "status":"hit"}),
            serde_json::json!({"event":"translation_cache", "status":"miss"}),
            serde_json::json!({"event":"diagnostic", "id":"cache_hit"}),
        ];
        assert_eq!(count_cache_events(&events, "hit"), 2);
        assert_eq!(count_cache_events(&events, "miss"), 1);
    }

    #[test]
    fn mixed_paragraph_is_eligible_when_any_unit_translates() {
        let value = serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 0,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 2.0, "top": 1.0},
                "text": {"chars": [
                    {"unicode":"1", "font_size":8.0, "baseline_origin":{"y":0.0}, "layout":{"label":"number", "policy":"passthrough"}},
                    {"unicode":"A", "font_size":8.0, "baseline_origin":{"y":0.0}, "layout":{"label":"text", "policy":"translate"}}
                ]}
            }]}]
        });
        let document: Il = serde_json::from_value(value).unwrap();
        assert!(is_translatable(&document.pages[0].paragraphs[0]));
    }

    #[test]
    fn whitespace_reconstruction_is_not_overtranslation() {
        assert!(text_equivalent("Google Brain", "GoogleBrain"));
    }

    #[test]
    fn translation_source_uses_the_public_implicit_space_contract() {
        let characters = [
            ('[', false),
            ('3', false),
            ('5', false),
            (',', false),
            ('2', true),
            (']', false),
            ('7', false),
            ('0', false),
            ('8', false),
            ('2', false),
        ]
        .into_iter()
        .map(|(character, implicit_space_before)| {
            serde_json::json!({
                "unicode": character.to_string(),
                "implicit_space_before": implicit_space_before,
                "font_size": 10.0,
                "baseline_origin": {"x": 0.0, "y": 0.0},
                "layout": {"label": "text", "policy": "translate"}
            })
        })
        .collect::<Vec<_>>();
        let document: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 1,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 1.0, "top": 1.0},
                "text": {"chars": characters}
            }]}]
        }))
        .unwrap();
        let source = translate_source_text(&document.pages[0].paragraphs[0]);
        assert_eq!(source, "[35, 2]7082");
        assert_eq!(conserved_tokens(&source), ["[35, 2]", "7082"]);
    }

    #[test]
    fn runtime_evidence_preserves_formula_boundaries_without_manufacturing_units() {
        let characters = [
            ("0", "text", "translate"),
            ("=", "inline_formula", "passthrough"),
            ("h", "text", "translate"),
        ]
        .into_iter()
        .map(|(character, label, policy)| {
            serde_json::json!({
                "unicode": character,
                "font_size": 10.0,
                "baseline_origin": {"x": 0.0, "y": 0.0},
                "layout": {"label": label, "policy": policy}
            })
        })
        .collect::<Vec<_>>();
        let before: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 1,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 1.0, "top": 1.0},
                "text": {"chars": characters}
            }]}]
        }))
        .unwrap();
        let without_evidence: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 1,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 1.0, "top": 1.0},
                "text": {"chars": []},
                "translated_text": "0"
            }]}]
        }))
        .unwrap();
        let with_evidence: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 1,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 1.0, "top": 1.0},
                "text": {"chars": []},
                "translated_text": "0",
                "translation_conservation": {
                    "request_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                    "response_sha256": "1111111111111111111111111111111111111111111111111111111111111111",
                    "source_token_types": 1,
                    "target_token_types": 1,
                    "source_tokens": [{"token": "0", "occurrences": 1}],
                    "target_tokens": [{"token": "0", "occurrences": 1}]
                }
            }]}]
        }))
        .unwrap();

        let reconstructed = conservation_measurement(&before, &without_evidence, None);
        assert_eq!(reconstructed.source_occurrences, 2);
        assert_eq!(reconstructed.missing_occurrences, 1);
        assert_eq!(reconstructed.violations[0].token, "h");

        let exact = conservation_measurement(&before, &with_evidence, None);
        assert_eq!(exact.source_occurrences, 1);
        assert_eq!(exact.preserved_occurrences, 1);
        assert_eq!(exact.missing_occurrences, 0);
        assert!(exact.violations.is_empty());
    }

    #[test]
    fn conservation_lexer_is_exact_and_conservative() {
        assert_eq!(
            conserved_tokens("At 3.5 days, 20 ms, 1e-3%, see [4,27,28,22]."),
            ["3.5", "20", "ms", "1e-3%", "[4,27,28,22]"]
        );
        assert!(conserved_tokens("one percent and model seven").is_empty());
    }

    #[test]
    fn conservation_normalizes_lexically_explicit_localized_quantities() {
        let source = conserved_tokens("36M, 4.5 million, 1/4, and 40K");
        let translated = conserved_tokens("3600 万、450 万、四分之一和 40,000");
        assert_eq!(source, ["36000000", "4500000", "1/4", "40000"]);
        assert_eq!(translated, source);
        assert_eq!(conserved_tokens("40K训练句子"), ["40000"]);
        assert!(conserved_tokens("40KB").contains(&"KB".to_owned()));
    }

    #[test]
    fn conservation_does_not_match_numeric_substrings() {
        let before: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 4.0, "top": 1.0},
                "text": {"chars": [
                    {"unicode":"4", "font_size":10.0, "baseline_origin":{"x":0.0,"y":0.0}, "box":{"left":0.0,"bottom":0.0,"right":1.0,"top":1.0}, "layout":{"label":"text","policy":"translate"}}
                ]}
            }]}]
        }))
        .unwrap();
        let after: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 4.0, "top": 1.0},
                "text": {"chars": []},
                "translated_text": "40"
            }]}]
        }))
        .unwrap();
        let result = conservation_measurement(&before, &after, None);
        assert_eq!(result.source_occurrences, 1);
        assert_eq!(result.missing_occurrences, 1);
    }

    #[test]
    fn glossary_requires_the_canonical_target_per_source_occurrence() {
        let before = il("translate", Some("source term and source term"), None);
        let after = il("translate", Some("规范译法和另一译法"), None);
        let glossary = Glossary {
            version: 1,
            terms: vec![GlossaryTerm {
                source: "A".into(),
                target: "规范译法".into(),
            }],
        };
        let result = terminology_measurement(&before, &after, &glossary, None);
        assert_eq!(result.source_occurrences, 1);
        assert_eq!(result.canonical_occurrences, 1);
        assert_eq!(result.violations, 0);

        let inconsistent = il("translate", Some("另一译法"), None);
        let result = terminology_measurement(&before, &inconsistent, &glossary, None);
        assert_eq!(result.violations, 1);
    }

    #[test]
    fn legacy_fake_conservation_is_not_applicable_and_unweighted() {
        let before = il("translate", None, None);
        let after = il("translate", Some("译"), None);
        let result = risk(
            &before,
            &after,
            &after,
            &[],
            EvaluationProfile::LegacyFake,
            None,
            &BTreeMap::new(),
            Path::new("missing-input.pdf"),
            Path::new("missing-output.pdf"),
            1.0,
        );
        assert_eq!(
            result.measurements["numeric_unit_reference_conservation"]["status"],
            "not-applicable"
        );
        assert_eq!(result.weighted_errors, 0.0);
    }

    #[test]
    fn split_formula_tail_is_a_critical_proxy_violation() {
        let before: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 4.0, "top": 1.0},
                "text": {"chars": [
                    {"unicode":"ϵ", "font_size":10.0, "baseline_origin":{"x":0.0,"y":0.0}, "box":{"left":0.0,"bottom":0.0,"right":1.0,"top":1.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":"l", "font_size":10.0, "baseline_origin":{"x":1.0,"y":0.0}, "box":{"left":1.0,"bottom":0.0,"right":2.0,"top":1.0}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"s", "font_size":10.0, "baseline_origin":{"x":2.0,"y":0.0}, "box":{"left":2.0,"bottom":0.0,"right":3.0,"top":1.0}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"]", "font_size":10.0, "baseline_origin":{"x":3.0,"y":0.0}, "box":{"left":3.0,"bottom":0.0,"right":4.0,"top":1.0}, "layout":{"label":"text","policy":"translate"}}
                ]}
            }]}]
        })).unwrap();
        let after: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 4.0, "top": 1.0},
                "text": {"chars": []},
                "translated_text": "值为ls]"
            }]}]
        }))
        .unwrap();
        let result = formula_completeness(&before, &after);
        assert_eq!(result.unbalanced_delimiter_paragraphs, 1);
        assert_eq!(result.adjacent_fragment_count, 1);
        assert_eq!(result.evidence[1].text, "ls]");
    }

    #[test]
    fn moved_formula_with_a_source_slot_stroke_is_an_orphan_ink_violation() {
        let styled: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 30.0, "right": 90.0, "top": 60.0},
                "text": {"chars": [
                    {"unicode":"A", "font_size":10.0, "baseline_origin":{"x":5.0,"y":40.0}, "box":{"left":5.0,"bottom":35.0,"right":10.0,"top":45.0}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"√", "font_size":10.0, "baseline_origin":{"x":40.0,"y":40.0}, "box":{"left":40.0,"bottom":35.0,"right":48.0,"top":48.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":"d", "font_size":10.0, "baseline_origin":{"x":48.0,"y":38.0}, "box":{"left":48.0,"bottom":34.0,"right":54.0,"top":44.0}, "layout":{"label":"inline_formula","policy":"passthrough"}}
                ]}
            }]}]
        }))
        .unwrap();
        let published: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 30.0, "right": 90.0, "top": 60.0},
                "text": {"chars": []},
                "translated_text": "译文"
            }]}]
        }))
        .unwrap();
        let source = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 0 100"><span><g unicode="√" x="40" y="40"/><g unicode="d" x="48" y="38"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 40 58"><moveto x="0" y="0"/><lineto x="14" y="0"/></stroke_path>
        </page></document>"#;
        let broken_trace = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 -25 100"><span><g unicode="√" x="40" y="40"/><g unicode="d" x="48" y="38"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 40 58"><moveto x="0" y="0"/><lineto x="14" y="0"/></stroke_path>
        </page></document>"#;
        let fixed_trace = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 -25 100"><span><g unicode="√" x="40" y="40"/><g unicode="d" x="48" y="38"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 15 58"><moveto x="0" y="0"/><lineto x="14" y="0"/></stroke_path>
        </page></document>"#;

        let broken =
            orphan_source_ink_from_traces(&styled, &published, source, broken_trace).unwrap();
        assert_eq!(broken.violations, 1);
        assert_eq!(broken.evidence[0].ink_kind, "vector_path");
        let fixed =
            orphan_source_ink_from_traces(&styled, &published, source, fixed_trace).unwrap();
        assert_eq!(fixed.violations, 0);

        let typed: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 30.0, "right": 90.0, "top": 60.0},
                "text": {"chars": []},
                "translated_text": null,
                "preserved": "typeset_protocol"
            }]}]
        }))
        .unwrap();
        let typed = orphan_source_ink_from_traces(&styled, &typed, source, broken_trace).unwrap();
        assert_eq!(typed.checked_formula_units, 0);
        assert_eq!(typed.violations, 0);
    }

    #[test]
    fn detached_source_order_radical_breaks_formula_rigid_body_integrity() {
        let styled: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 4,
                "bounds": {"left": 0.0, "bottom": 30.0, "right": 90.0, "top": 60.0},
                "text": {"chars": [
                    {"unicode":"A", "font_size":10.0, "baseline_origin":{"x":5.0,"y":40.0}, "box":{"left":5.0,"bottom":35.0,"right":10.0,"top":45.0}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"√", "font_size":10.0, "baseline_origin":{"x":40.0,"y":40.0}, "box":{"left":40.0,"bottom":35.0,"right":48.0,"top":48.0}, "visual_bbox":{"left":40.0,"bottom":35.0,"right":48.0,"top":48.0}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"d", "font_size":10.0, "baseline_origin":{"x":48.0,"y":38.0}, "box":{"left":48.0,"bottom":34.0,"right":54.0,"top":44.0}, "visual_bbox":{"left":48.0,"bottom":34.0,"right":54.0,"top":44.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":"k", "font_size":7.0, "baseline_origin":{"x":54.0,"y":36.0}, "box":{"left":54.0,"bottom":33.0,"right":58.0,"top":40.0}, "visual_bbox":{"left":54.0,"bottom":33.0,"right":58.0,"top":40.0}, "layout":{"label":"inline_formula","policy":"passthrough"}}
                ]}
            }]}]
        }))
        .unwrap();
        let published: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 4,
                "bounds": {"left": 0.0, "bottom": 30.0, "right": 90.0, "top": 60.0},
                "text": {"chars": []},
                "translated_text": "译文"
            }]}]
        }))
        .unwrap();
        let source = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 0 100"><span><g unicode="√" x="40" y="40"/><g unicode="d" x="48" y="38"/><g unicode="k" x="54" y="36"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 48 56"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;
        let broken = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 -25 100"><span><g unicode="d" x="48" y="38"/><g unicode="k" x="54" y="36"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 23 56"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;
        let fixed = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 -25 100"><span><g unicode="√" x="40" y="40"/><g unicode="d" x="48" y="38"/><g unicode="k" x="54" y="36"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 23 56"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;

        let units = formula_units(&styled);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].text, "√dk");
        assert!(units[0].has_attached_source_radical);

        let broken =
            formula_rigid_body_integrity_from_traces(&styled, &published, source, broken).unwrap();
        assert_eq!(broken.checked_formula_units, 1);
        assert_eq!(broken.violations, 1);
        let fixed =
            formula_rigid_body_integrity_from_traces(&styled, &published, source, fixed).unwrap();
        assert_eq!(fixed.checked_formula_units, 1);
        assert_eq!(fixed.violations, 0);
    }

    #[test]
    fn formula_ink_is_audited_only_for_its_unique_geometric_owner() {
        let styled: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 4,
                "bounds": {"left": 10.0, "bottom": 20.0, "right": 90.0, "top": 50.0},
                "text": {"chars": [
                    {"unicode":"x", "font_size":10.0, "baseline_origin":{"x":40.0,"y":40.0}, "box":{"left":40.0,"bottom":35.0,"right":45.0,"top":45.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":"y", "font_size":10.0, "baseline_origin":{"x":46.0,"y":40.0}, "box":{"left":46.0,"bottom":35.0,"right":51.0,"top":45.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":".", "font_size":10.0, "baseline_origin":{"x":52.0,"y":40.0}, "box":{"left":52.0,"bottom":35.0,"right":54.0,"top":45.0}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"u", "font_size":10.0, "baseline_origin":{"x":40.0,"y":30.0}, "box":{"left":40.0,"bottom":25.0,"right":45.0,"top":35.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":"v", "font_size":10.0, "baseline_origin":{"x":46.0,"y":30.0}, "box":{"left":46.0,"bottom":25.0,"right":51.0,"top":35.0}, "layout":{"label":"inline_formula","policy":"passthrough"}}
                ]}
            }]}]
        }))
        .unwrap();
        let published: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 4,
                "bounds": {"left": 10.0, "bottom": 20.0, "right": 90.0, "top": 50.0},
                "text": {"chars": []},
                "translated_text": "translated"
            }]}]
        }))
        .unwrap();
        let source = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 0 100"><span><g unicode="x" x="40" y="40"/><g unicode="y" x="46" y="40"/><g unicode="u" x="40" y="30"/><g unicode="v" x="46" y="30"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 40 65"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;
        let output = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 -10 100"><span><g unicode="x" x="40" y="40"/><g unicode="y" x="46" y="40"/></span></fill_text>
          <fill_text transform="1 0 0 -1 0 100"><span><g unicode="u" x="40" y="30"/><g unicode="v" x="46" y="30"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 40 65"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;

        let rigid =
            formula_rigid_body_integrity_from_traces(&styled, &published, source, output).unwrap();
        assert_eq!(rigid.checked_formula_units, 1);
        assert_eq!(rigid.violations, 0);
        let orphan = orphan_source_ink_from_traces(&styled, &published, source, output).unwrap();
        assert_eq!(orphan.violations, 0);
    }

    #[test]
    fn formula_rigid_body_anchor_may_include_half_em_ink_extent() {
        let styled: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 4,
                "bounds": {"left": 10.0, "bottom": 20.0, "right": 90.0, "top": 50.0},
                "text": {"chars": [
                    {"unicode":"u", "font_size":10.0, "baseline_origin":{"x":40.0,"y":40.0}, "box":{"left":40.0,"bottom":35.0,"right":45.0,"top":45.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":"v", "font_size":10.0, "baseline_origin":{"x":46.0,"y":40.0}, "box":{"left":46.0,"bottom":35.0,"right":51.0,"top":45.0}, "layout":{"label":"inline_formula","policy":"passthrough"}}
                ]}
            }]}]
        }))
        .unwrap();
        let published: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 4,
                "bounds": {"left": 10.0, "bottom": 20.0, "right": 90.0, "top": 50.0},
                "text": {"chars": []},
                "translated_text": "translated"
            }]}]
        }))
        .unwrap();
        let source = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 0 100"><span><g unicode="u" x="40" y="40"/><g unicode="v" x="46" y="40"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 40 55"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;
        let output = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 -32.6 100"><span><g unicode="u" x="40" y="40"/><g unicode="v" x="46" y="40"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 7.4 55"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;

        let rigid =
            formula_rigid_body_integrity_from_traces(&styled, &published, source, output).unwrap();
        assert_eq!(rigid.checked_formula_units, 1);
        assert_eq!(rigid.violations, 0);
    }

    #[test]
    fn formula_rigid_body_anchor_uses_published_admissible_expansion() {
        let styled: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 4,
                "bounds": {"left": 10.0, "bottom": 20.0, "right": 90.0, "top": 50.0},
                "text": {"chars": [
                    {"unicode":"u", "font_size":10.0, "baseline_origin":{"x":40.0,"y":40.0}, "box":{"left":40.0,"bottom":35.0,"right":45.0,"top":45.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":"v", "font_size":10.0, "baseline_origin":{"x":46.0,"y":40.0}, "box":{"left":46.0,"bottom":35.0,"right":51.0,"top":45.0}, "layout":{"label":"inline_formula","policy":"passthrough"}}
                ]}
            }]}]
        }))
        .unwrap();
        let published: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "geometry": {"height": 100.0}, "paragraphs": [{
                "reading_order": 4,
                "bounds": {"left": 10.0, "bottom": 20.0, "right": 90.0, "top": 50.0},
                "text": {"chars": []},
                "translated_text": "translated"
            }]}],
            "publication_ink": [{
                "page_index": 0,
                "reading_order": 4,
                "admissible_container": {"left": 10.0, "bottom": 5.0, "right": 90.0, "top": 50.0}
            }]
        }))
        .unwrap();
        let source = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 0 100"><span><g unicode="u" x="40" y="40"/><g unicode="v" x="46" y="40"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 40 55"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;
        let output = r#"<document><page number="1" mediabox="0 0 100 100">
          <fill_text transform="1 0 0 -1 0 130"><span><g unicode="u" x="40" y="40"/><g unicode="v" x="46" y="40"/></span></fill_text>
          <stroke_path linewidth="0.4" transform="1 0 0 -1 40 85"><moveto x="0" y="0"/><lineto x="10" y="0"/></stroke_path>
        </page></document>"#;

        let rigid =
            formula_rigid_body_integrity_from_traces(&styled, &published, source, output).unwrap();
        assert_eq!(rigid.checked_formula_units, 1);
        assert_eq!(rigid.violations, 0);
    }

    #[test]
    fn continuity_bound_excludes_formula_adjacent_source_gaps() {
        let document: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 202.0, "top": 1.0},
                "text": {"chars": [
                    {"unicode":"A", "font_size":10.0, "baseline_origin":{"x":0.0,"y":0.0}, "box":{"left":0.0,"bottom":0.0,"right":1.0,"top":1.0}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"B", "implicit_space_before":true, "font_size":10.0, "baseline_origin":{"x":3.0,"y":0.0}, "box":{"left":3.0,"bottom":0.0,"right":4.0,"top":1.0}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"x", "font_size":10.0, "baseline_origin":{"x":100.0,"y":0.0}, "box":{"left":100.0,"bottom":0.0,"right":101.0,"top":1.0}, "layout":{"label":"inline_formula","policy":"passthrough"}},
                    {"unicode":"C", "implicit_space_before":true, "font_size":10.0, "baseline_origin":{"x":201.0,"y":0.0}, "box":{"left":201.0,"bottom":0.0,"right":202.0,"top":1.0}, "layout":{"label":"text","policy":"translate"}}
                ]}
            }]}]
        }))
        .unwrap();
        assert_eq!(continuity_bound(&document.pages[0].paragraphs[0]), 15.0);
    }

    #[test]
    fn extraction_order_punctuation_joins_the_complete_formula_audit_unit() {
        // Unlike the ordinary adjacent formula shape at (4,21), (3,9) is extracted
        // as `formula sqrt`, sentence punctuation, then the geometrically preceding
        // `formula d_k`. Production moves that punctuation after the complete unit.
        let character = |unicode: &str,
                         left: f64,
                         bottom: f64,
                         right: f64,
                         top: f64,
                         label: &str,
                         policy: &str| {
            serde_json::json!({
                "unicode": unicode,
                "font_size": 10.0,
                "baseline_origin": {"x": left, "y": 10.0},
                "box": {"left": left, "bottom": bottom, "right": right, "top": top},
                "visual_bbox": {"left": left, "bottom": bottom, "right": right, "top": top},
                "layout": {"label": label, "policy": policy}
            })
        };
        let document: Il = serde_json::from_value(serde_json::json!({
            "pages": [{
                "index": 3,
                "geometry": {"height": 100.0},
                "paragraphs": [{
                    "reading_order": 9,
                    "bounds": {"left": 0.0, "bottom": 5.0, "right": 28.0, "top": 15.0},
                    "text": {"chars": [
                        character("A", 0.0, 5.0, 5.0, 15.0, "text", "translate"),
                        character("√", 10.0, 5.0, 16.0, 15.0, "inline_formula", "passthrough"),
                        character(".", 26.0, 5.0, 28.0, 15.0, "text", "translate"),
                        character("d", 16.0, 5.0, 21.0, 15.0, "inline_formula", "passthrough"),
                        character("k", 21.0, 3.0, 25.0, 10.0, "inline_formula", "passthrough")
                    ]}
                }]
            }]
        }))
        .unwrap();

        let units = formula_units(&document);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].text, "√dk");
        assert!(units[0].expects_left_neighbor);
        assert!(units[0].expects_right_neighbor);
    }

    #[test]
    fn model_owned_formula_tail_joins_across_the_intervening_visual_line() {
        let region = serde_json::json!({
            "label": "inline_formula",
            "reading_order": 99,
            "bounds": {"left": 10.0, "bottom": 3.0, "right": 25.0, "top": 15.0},
            "source": "model",
            "policy": "passthrough"
        });
        let character =
            |unicode: &str, left: f64, bottom: f64, right: f64, top: f64, layout: Value| {
                serde_json::json!({
                    "unicode": unicode,
                    "font_size": 10.0,
                    "baseline_origin": {"x": left, "y": 10.0},
                    "box": {"left": left, "bottom": bottom, "right": right, "top": top},
                    "visual_bbox": {"left": left, "bottom": bottom, "right": right, "top": top},
                    "layout": layout
                })
            };
        let text_layout = || {
            serde_json::json!({
                "label": "text",
                "reading_order": 1,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 60.0, "top": 20.0},
                "source": "model",
                "policy": "translate"
            })
        };
        let document: Il = serde_json::from_value(serde_json::json!({
            "pages": [{
                "index": 3,
                "geometry": {"height": 100.0},
                "paragraphs": [{
                    "reading_order": 8,
                    "bounds": {"left": 0.0, "bottom": 0.0, "right": 60.0, "top": 20.0},
                    "text": {"chars": [
                        character("A", 0.0, 5.0, 5.0, 15.0, text_layout()),
                        character("√", 10.0, 5.0, 16.0, 15.0, region.clone()),
                        character(".", 26.0, 5.0, 28.0, 15.0, text_layout()),
                        character("after", 28.0, 5.0, 55.0, 15.0, text_layout()),
                        character("d", 16.0, 5.0, 21.0, 15.0, region.clone()),
                        character("k", 21.0, 3.0, 25.0, 10.0, region.clone()),
                        character("tail", 0.0, 0.0, 20.0, 2.0, text_layout())
                    ]}
                }]
            }]
        }))
        .unwrap();

        let units = formula_units(&document);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].text, "√dk");
        assert!(units[0].expects_left_neighbor);
        assert!(units[0].expects_right_neighbor);
    }

    #[test]
    fn complete_formula_matches_adjacent_split_stext_glyph_lines() {
        let lines = vec![
            StextLine {
                bbox: StextBox {
                    x: 0.0,
                    y: 10.0,
                    w: 50.0,
                    h: 8.0,
                },
                text: "translated".into(),
            },
            StextLine {
                bbox: StextBox {
                    x: 50.0,
                    y: 11.0,
                    w: 6.0,
                    h: 6.0,
                },
                text: "√".into(),
            },
            StextLine {
                bbox: StextBox {
                    x: 56.0,
                    y: 8.0,
                    w: 4.0,
                    h: 4.0,
                },
                text: "d".into(),
            },
            StextLine {
                bbox: StextBox {
                    x: 60.0,
                    y: 11.0,
                    w: 3.0,
                    h: 3.0,
                },
                text: "k".into(),
            },
            StextLine {
                bbox: StextBox {
                    x: 63.0,
                    y: 7.0,
                    w: 30.0,
                    h: 8.0,
                },
                text: "缩放点积。".into(),
            },
        ];

        assert_eq!(
            match_formula_lines(&lines, &[], "√dk", 13.0, 10.0),
            Some(vec![1, 2, 3])
        );
        assert_eq!(
            match_formula_lines(&lines, &[], "√", 13.0, 10.0),
            Some(vec![1])
        );
    }

    #[test]
    fn formula_neighbors_use_visual_overlap_for_split_superscript_lines() {
        let line = |x, y, w, h, text: &str| StextLine {
            bbox: StextBox { x, y, w, h },
            text: text.into(),
        };
        // MuPDF's stext.json shape for anchor paragraph (6,10): the epsilon and
        // equals spans overlap the formula visually even though their top edges
        // differ by 3pt from the compact signed-superscript formula union.
        let lines = vec![
            line(349.0, 578.0, 14.0, 7.0, " 且 "),
            line(364.0, 580.0, 6.0, 5.0, "ϵ"),
            line(370.0, 580.0, 7.0, 4.0, " ="),
            line(386.0, 579.0, 9.0, 6.0, "10"),
            line(396.0, 580.0, 6.0, 0.0, "−"),
            line(402.0, 577.0, 3.0, 4.0, "9"),
            line(407.0, 577.0, 89.0, 9.0, "。在训练过程中，我"),
        ];
        let formula_bbox = lines[3..=5]
            .iter()
            .map(|line| line.bbox)
            .reduce(stext_box_union)
            .unwrap();

        assert_eq!(
            formula_neighbor_gaps(&lines, &[3, 4, 5], formula_bbox),
            (Some(9.0), Some(2.0))
        );
    }

    #[test]
    fn translated_title_and_complete_author_block_fail_policy_conservation() {
        let before: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [
                test_paragraph(2, "doc_title", "translate", "Title"),
                test_paragraph(3, "text", "translate", "Author Institution email@example.com"),
                test_paragraph(4, "abstract", "translate", "Abstract body")
            ]}]
        }))
        .unwrap();
        let result = title_author_conservation(&before, &before).unwrap();
        assert_eq!(result.author_paragraphs, 1);
        assert!(!result.title_policy_passthrough);
        assert!(!result.author_policy_passthrough);
        assert_eq!(result.failures, 2);
    }

    #[test]
    fn title_author_conservation_uses_geometry_when_authors_follow_abstract_in_reading_order() {
        let before: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [
                test_paragraph_at(0, "doc_title", "passthrough", "Title", 90.0),
                test_paragraph_at(1, "text", "passthrough", "email@example.com", 70.0),
                test_paragraph_at(2, "abstract", "translate", "Abstract", 30.0),
                test_paragraph_at(3, "text", "translate", "Body outside band", 10.0),
                test_paragraph_at(11, "fallback_line", "translate", "Author names", 60.0),
                test_paragraph_at(12, "fallback_line", "translate", "Institution", 50.0)
            ]}]
        }))
        .unwrap();

        let result = title_author_conservation(&before, &before).unwrap();
        assert_eq!(result.author_paragraphs, 3);
        assert_eq!(
            (result.band_lower, result.band_upper, result.band_tolerance),
            (27.0, 93.0, 4.0)
        );
        assert!(!result.author_policy_passthrough);
        assert!(result.title_policy_passthrough);
        assert_eq!(result.failures, 1);
    }

    #[test]
    fn title_author_hash_and_identity_include_the_visual_box() {
        let document = || {
            serde_json::from_value::<Il>(serde_json::json!({
                "pages": [{"index": 0, "paragraphs": [
                    test_paragraph(2, "doc_title", "passthrough", "Title"),
                    test_paragraph(3, "text", "passthrough", "Author Institution email@example.com"),
                    test_paragraph(4, "abstract", "translate", "Abstract body")
                ]}]
            }))
            .unwrap()
        };
        let source = document();
        let unchanged = title_author_conservation(&source, &source).unwrap();
        assert_eq!(
            unchanged.write_hash_sha256.as_deref(),
            Some(unchanged.source_hash_sha256.as_str())
        );

        let mut changed = document();
        changed.pages[0].paragraphs[0].text.chars[0].visual_bbox = Some(Rect {
            left: 100.0,
            bottom: 100.0,
            right: 101.0,
            top: 101.0,
        });
        let changed = title_author_conservation(&source, &changed).unwrap();
        assert!(!changed.title_write_identity);
        assert_ne!(
            changed.write_hash_sha256.as_deref(),
            Some(changed.source_hash_sha256.as_str())
        );
    }

    #[test]
    fn conservation_excludes_form_xobject_characters_not_in_page_contents() {
        let before: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 4.0, "top": 1.0},
                "text": {"chars": [
                    {"unicode":"4", "font_size":10.0, "baseline_origin":{"x":0.0,"y":0.0}, "text_transform":{"kind":"upright"}, "passthrough":{"content_object":18}, "layout":{"label":"text","policy":"translate"}},
                    {"unicode":"0", "font_size":10.0, "baseline_origin":{"x":1.0,"y":0.0}, "text_transform":{"kind":"upright"}, "passthrough":{"content_object":51}, "layout":{"label":"text","policy":"translate"}}
                ]}
            }]}]
        }))
        .unwrap();
        let after: Il = serde_json::from_value(serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 2,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 4.0, "top": 1.0},
                "text": {"chars": []},
                "translated_text": "译4"
            }]}]
        }))
        .unwrap();
        let direct = BTreeMap::from([(0, BTreeSet::from([18]))]);
        let result = conservation_measurement(&before, &after, Some(&direct));
        assert_eq!(result.source_occurrences, 1);
        assert_eq!(result.preserved_occurrences, 1);
        assert_eq!(result.missing_occurrences, 0);
    }

    #[test]
    fn qpdf_page_json_yields_direct_content_object_numbers() {
        let value = serde_json::json!({
            "pages": [
                {"contents": ["18 0 R", "2083 2 R"]},
                {"contents": []}
            ]
        });
        let result = direct_content_objects_from_qpdf_json(&value).unwrap();
        assert_eq!(result[&0], BTreeSet::from([18, 2083]));
        assert!(result[&1].is_empty());
    }

    #[test]
    fn resource_time_accepts_macos_and_prefix_formats() {
        assert_eq!(
            parse_resource_f64("11.40 real 17.86 user 0.63 sys\n", "real"),
            Some(11.4)
        );
        assert_eq!(parse_resource_f64("real 11.40\n", "real"), Some(11.4));
    }

    fn test_paragraph(reading_order: usize, label: &str, policy: &str, text: &str) -> Value {
        test_paragraph_at(
            reading_order,
            label,
            policy,
            text,
            100.0 - reading_order as f64 * 20.0,
        )
    }

    fn test_paragraph_at(
        reading_order: usize,
        label: &str,
        policy: &str,
        text: &str,
        baseline: f64,
    ) -> Value {
        let chars = text
            .chars()
            .enumerate()
            .map(|(index, c)| serde_json::json!({
                "unicode": c.to_string(),
                "font_size": 8.0,
                "baseline_origin": {"x": index as f64, "y": baseline},
                "box": {"left": index as f64, "bottom": baseline - 1.0, "right": index as f64 + 1.0, "top": baseline + 1.0},
                "visual_bbox": {"left": index as f64, "bottom": baseline - 1.0, "right": index as f64 + 1.0, "top": baseline + 1.0},
                "layout": {"label": label, "policy": policy}
            }))
            .collect::<Vec<_>>();
        serde_json::json!({
            "reading_order": reading_order,
            "bounds": {"left": 0.0, "bottom": baseline - 1.0, "right": text.len() as f64, "top": baseline + 1.0},
            "text": {"chars": chars}
        })
    }
}
