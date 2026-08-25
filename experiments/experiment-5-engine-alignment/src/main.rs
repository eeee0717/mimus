use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use lopdf::Document as LopdfDocument;
use mimus_core::engine::pdfium::PdfiumEngine;
use mimus_core::engine::{PageCharSnapshot, PdfInspector};
use mimus_core::il::{PageGeometry, TextTransform};
use mimus_core::walk::{UnicodeProvenance, WalkedChar, walk_page};
use serde::Serialize;
use sha2::{Digest, Sha256};

const DEFAULT_TOLERANCES: &str = "0.001,0.005,0.01,0.05,0.1,0.25,0.5";
const EXACT_TOLERANCE_PT: f64 = 0.001;
const SEQUENCE_ANCHOR_DISTANCE_PT: f64 = 36.0;

#[derive(Debug, Parser)]
#[command(about = "Classify operator-walk/PDFium character residuals without changing a PDF")]
struct Args {
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    #[arg(long, env = "MIMUS_PDFIUM_LIBRARY")]
    pdfium_library: PathBuf,

    #[arg(long)]
    output: PathBuf,

    #[arg(long, default_value = DEFAULT_TOLERANCES)]
    tolerances: String,

    #[arg(long, default_value_t = 200)]
    detail_limit_per_page: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    exact_tolerance_pt: f64,
    pdfium_generated_characters: &'static str,
    tolerances_pt: Vec<f64>,
    documents: Vec<DocumentReport>,
    totals_by_tolerance: Vec<ToleranceSummary>,
}

#[derive(Debug, Serialize)]
struct DocumentReport {
    path: String,
    sha256: Option<String>,
    bytes: Option<usize>,
    pages: usize,
    error: Option<String>,
    primary_pages: Vec<PageClassification>,
    totals_by_tolerance: Vec<ToleranceSummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ToleranceSummary {
    tolerance_pt: f64,
    pages: usize,
    walked_characters: usize,
    engine_characters: usize,
    geometrically_matched: usize,
    exact_matches: usize,
    reconciled_same_unicode: usize,
    reconciled_unique_mismatch: usize,
    sequence_only_correspondences: usize,
    same_unicode: usize,
    class_a: usize,
    class_a_engine_outside_page: usize,
    class_b_moved_pairs: usize,
    class_b_inversions: u64,
    class_c: usize,
    class_c_pdfium_hyphen: usize,
    class_c_pdfium_utf16_surrogate: usize,
    class_c_pdfium_ligature_expansion: usize,
    class_c_strong_other: usize,
    class_c_weak_other: usize,
    class_c_unresolved: usize,
    class_d: usize,
    class_e: usize,
    class_f: usize,
    ambiguous_walk_nodes: usize,
    ambiguous_engine_nodes: usize,
    max_delta_x_pt: f64,
    max_delta_y_pt: f64,
}

#[derive(Debug, Serialize)]
struct PageClassification {
    page_index: usize,
    tolerance_pt: f64,
    walked_characters: usize,
    engine_characters: usize,
    geometrically_matched: usize,
    exact_matches: usize,
    reconciled_same_unicode: usize,
    reconciled_unique_mismatch: usize,
    sequence_only_correspondences: usize,
    same_unicode: usize,
    class_a: usize,
    class_a_engine_outside_page: usize,
    class_b_moved_pairs: usize,
    class_b_inversions: u64,
    class_c: usize,
    class_c_pdfium_hyphen: usize,
    class_c_pdfium_utf16_surrogate: usize,
    class_c_pdfium_ligature_expansion: usize,
    class_c_strong_other: usize,
    class_c_weak_other: usize,
    class_c_unresolved: usize,
    class_d: usize,
    class_e: usize,
    class_f: usize,
    ambiguous_walk_nodes: usize,
    ambiguous_engine_nodes: usize,
    max_delta_x_pt: f64,
    max_delta_y_pt: f64,
    recoveries: Vec<String>,
    error: Option<String>,
    residuals_truncated: bool,
    residuals: Vec<Residual>,
}

#[derive(Debug, Serialize)]
struct Residual {
    class: &'static str,
    reason: &'static str,
    match_stage: Option<&'static str>,
    walk: Option<WalkCharacter>,
    engine: Option<EngineCharacter>,
    delta_x_pt: Option<f64>,
    delta_y_pt: Option<f64>,
}

#[derive(Debug, Serialize)]
struct WalkCharacter {
    index: usize,
    unicode: Option<String>,
    unicode_value: Option<u32>,
    unicode_provenance: &'static str,
    code: u32,
    encoded_hex: String,
    visible: bool,
    locatable: bool,
    font_supported: bool,
    engine_mismatch_tolerated: bool,
    text_transform: String,
    font_resource: String,
    font_object: u32,
    font_generation: u16,
    baseline_x: f64,
    baseline_y: f64,
    metric_left: f64,
    metric_bottom: f64,
    metric_right: f64,
    metric_top: f64,
    content_object: u32,
    content_generation: u16,
    byte_start: usize,
    byte_end: usize,
}

#[derive(Debug, Serialize)]
struct EngineCharacter {
    array_index: usize,
    pdfium_index: u32,
    unicode: Option<String>,
    unicode_value: u32,
    is_hyphen: Option<bool>,
    baseline_x: f64,
    baseline_y: f64,
    tight_left: f64,
    tight_bottom: f64,
    tight_right: f64,
    tight_top: f64,
}

#[derive(Debug, Clone, Copy)]
struct Pair {
    walk: usize,
    engine: usize,
    delta_x: f64,
    delta_y: f64,
    stage: MatchStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchStage {
    Exact,
    ReconciledSameUnicode,
    ReconciledUniqueMismatch,
}

#[derive(Debug)]
struct MatchResult {
    pairs: Vec<Pair>,
    unmatched_walk: Vec<usize>,
    unmatched_engine: Vec<usize>,
    walk_candidate_counts: Vec<usize>,
    engine_candidate_counts: Vec<usize>,
    sequence_only_correspondences: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let tolerances = parse_tolerances(&args.tolerances)?;
    let paths = collect_pdf_paths(&args.inputs)?;
    if paths.is_empty() {
        bail!("no PDF inputs found");
    }
    let engine = PdfiumEngine::new(&args.pdfium_library)?;
    let mut documents = Vec::with_capacity(paths.len());
    for path in paths {
        eprintln!("classifying {}", path.display());
        documents.push(classify_document(
            &path,
            &engine,
            &tolerances,
            args.detail_limit_per_page,
        ));
    }
    let totals_by_tolerance = aggregate_document_summaries(&documents, &tolerances);
    let report = Report {
        schema_version: 2,
        exact_tolerance_pt: EXACT_TOLERANCE_PT,
        pdfium_generated_characters: "excluded by PdfiumEngine::page_characters",
        tolerances_pt: tolerances,
        documents,
        totals_by_tolerance,
    };
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(&report)?;
    fs::write(&args.output, json)
        .with_context(|| format!("could not write {}", args.output.display()))?;
    eprintln!("wrote {}", args.output.display());
    Ok(())
}

fn parse_tolerances(value: &str) -> Result<Vec<f64>> {
    let parsed = value
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f64>()
                .with_context(|| format!("invalid tolerance {part:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if parsed.is_empty()
        || parsed
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
    {
        bail!("tolerances must be finite positive numbers");
    }
    let mut tolerances = Vec::with_capacity(parsed.len());
    for value in parsed {
        if !tolerances
            .iter()
            .any(|existing: &f64| existing.total_cmp(&value).is_eq())
        {
            tolerances.push(value);
        }
    }
    Ok(tolerances)
}

fn collect_pdf_paths(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for input in inputs {
        collect_pdf_path(input, &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_pdf_path(path: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_file() {
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        {
            output.push(path.to_path_buf());
        }
        return Ok(());
    }
    if !path.is_dir() {
        bail!("input does not exist: {}", path.display());
    }
    for entry in fs::read_dir(path).with_context(|| format!("could not read {}", path.display()))? {
        collect_pdf_path(&entry?.path(), output)?;
    }
    Ok(())
}

fn classify_document(
    path: &Path,
    engine: &PdfiumEngine,
    tolerances: &[f64],
    detail_limit: usize,
) -> DocumentReport {
    match try_classify_document(path, engine, tolerances, detail_limit) {
        Ok(report) => report,
        Err(error) => DocumentReport {
            path: path.display().to_string(),
            sha256: None,
            bytes: None,
            pages: 0,
            error: Some(format!("{error:#}")),
            primary_pages: Vec::new(),
            totals_by_tolerance: tolerances
                .iter()
                .map(|&tolerance_pt| ToleranceSummary {
                    tolerance_pt,
                    ..ToleranceSummary::default()
                })
                .collect(),
        },
    }
}

fn try_classify_document(
    path: &Path,
    engine: &PdfiumEngine,
    tolerances: &[f64],
    detail_limit: usize,
) -> Result<DocumentReport> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let document = LopdfDocument::load_mem(&bytes)
        .with_context(|| format!("lopdf could not parse {}", path.display()))?;
    let pages = document.get_pages();
    let engine_page_count = engine.page_count(&bytes)?;
    if pages.len() != engine_page_count {
        bail!(
            "lopdf found {} pages but PDFium found {engine_page_count}",
            pages.len()
        );
    }

    let mut primary_pages = Vec::with_capacity(pages.len());
    let mut totals_by_tolerance = tolerances
        .iter()
        .map(|&tolerance_pt| ToleranceSummary {
            tolerance_pt,
            ..ToleranceSummary::default()
        })
        .collect::<Vec<_>>();

    for (page_index, (_, page_id)) in pages.into_iter().enumerate() {
        let walked = match walk_page(&document, page_id) {
            Ok(walked) => walked,
            Err(error) => {
                primary_pages.push(error_page(page_index, tolerances[0], format!("{error}")));
                continue;
            }
        };
        let engine_characters =
            match engine.page_characters_with_text_diagnostics(&bytes, page_index) {
                Ok(characters) => characters,
                Err(error) => {
                    primary_pages.push(error_page(page_index, tolerances[0], format!("{error}")));
                    continue;
                }
            };
        let page_geometry = match engine.page_geometry(&bytes, page_index) {
            Ok(geometry) => geometry,
            Err(error) => {
                primary_pages.push(error_page(page_index, tolerances[0], format!("{error}")));
                continue;
            }
        };

        for (index, &tolerance) in tolerances.iter().enumerate() {
            let classification = classify_page(
                page_index,
                page_geometry,
                &walked.characters,
                &engine_characters,
                tolerance,
                &walked
                    .recoveries
                    .iter()
                    .map(|value| format!("{value:?}"))
                    .collect::<Vec<_>>(),
                if index == 0 { detail_limit } else { 0 },
            );
            totals_by_tolerance[index].add_page(&classification);
            if index == 0 {
                primary_pages.push(classification);
            }
        }
    }

    Ok(DocumentReport {
        path: path.display().to_string(),
        sha256: Some(sha256),
        bytes: Some(bytes.len()),
        pages: engine_page_count,
        error: None,
        primary_pages,
        totals_by_tolerance,
    })
}

fn error_page(page_index: usize, tolerance_pt: f64, error: String) -> PageClassification {
    PageClassification {
        page_index,
        tolerance_pt,
        walked_characters: 0,
        engine_characters: 0,
        geometrically_matched: 0,
        exact_matches: 0,
        reconciled_same_unicode: 0,
        reconciled_unique_mismatch: 0,
        sequence_only_correspondences: 0,
        same_unicode: 0,
        class_a: 0,
        class_a_engine_outside_page: 0,
        class_b_moved_pairs: 0,
        class_b_inversions: 0,
        class_c: 0,
        class_c_pdfium_hyphen: 0,
        class_c_pdfium_utf16_surrogate: 0,
        class_c_pdfium_ligature_expansion: 0,
        class_c_strong_other: 0,
        class_c_weak_other: 0,
        class_c_unresolved: 0,
        class_d: 0,
        class_e: 0,
        class_f: 0,
        ambiguous_walk_nodes: 0,
        ambiguous_engine_nodes: 0,
        max_delta_x_pt: 0.0,
        max_delta_y_pt: 0.0,
        recoveries: Vec::new(),
        error: Some(error),
        residuals_truncated: false,
        residuals: Vec::new(),
    }
}

fn classify_page(
    page_index: usize,
    page_geometry: PageGeometry,
    walked: &[WalkedChar],
    engine: &[PageCharSnapshot],
    tolerance: f64,
    recoveries: &[String],
    detail_limit: usize,
) -> PageClassification {
    let matches = match_staged(walked, engine, tolerance);
    let mut residuals = Vec::new();
    let mut residual_count = 0usize;
    let mut class_a = 0usize;
    let mut class_a_engine_outside_page = 0usize;
    let mut class_c = 0usize;
    let mut class_c_pdfium_hyphen = 0usize;
    let mut class_c_pdfium_utf16_surrogate = 0usize;
    let mut class_c_pdfium_ligature_expansion = 0usize;
    let mut class_c_strong_other = 0usize;
    let mut class_c_weak_other = 0usize;
    let mut class_c_unresolved = 0usize;
    let mut class_d = 0usize;
    let mut class_e = 0usize;
    let mut class_f = 0usize;
    let mut same_unicode = 0usize;
    let mut max_delta_x_pt = 0.0f64;
    let mut max_delta_y_pt = 0.0f64;
    let mut order_pairs = Vec::new();

    for pair in &matches.pairs {
        let walk = &walked[pair.walk];
        let engine_character = &engine[pair.engine];
        max_delta_x_pt = max_delta_x_pt.max(pair.delta_x);
        max_delta_y_pt = max_delta_y_pt.max(pair.delta_y);
        let whitespace = walk.unicode.is_some_and(char::is_whitespace)
            || engine_character.unicode.is_some_and(char::is_whitespace);
        if walk.unicode == engine_character.unicode {
            same_unicode += 1;
            if !whitespace && walk.visible {
                order_pairs.push(*pair);
            }
            continue;
        }
        let (class, reason) = if whitespace || !walk.visible {
            class_a += 1;
            ("A", "matched ignored whitespace or invisible character")
        } else {
            class_c += 1;
            if is_pdfium_hyphen_marker(walk, engine_character) {
                class_c_pdfium_hyphen += 1;
            } else if is_pdfium_utf16_surrogate(walk, engine_character) {
                class_c_pdfium_utf16_surrogate += 1;
            } else if is_pdfium_ligature_expansion(walk, engine_character) {
                class_c_pdfium_ligature_expansion += 1;
            } else {
                match walk.unicode_provenance {
                    UnicodeProvenance::ToUnicode | UnicodeProvenance::EmbeddedFontCmap => {
                        class_c_strong_other += 1;
                    }
                    UnicodeProvenance::SimpleEncoding => class_c_weak_other += 1,
                    UnicodeProvenance::Unresolved => class_c_unresolved += 1,
                }
            }
            ("C", "same geometry but different Unicode")
        };
        push_residual(
            &mut residuals,
            &mut residual_count,
            detail_limit,
            residual_pair(class, reason, pair, walked, engine),
        );
    }

    for &index in &matches.unmatched_walk {
        let character = &walked[index];
        let (class, reason) =
            if character.unicode.is_some_and(char::is_whitespace) || !character.visible {
                class_a += 1;
                ("A", "walk-only whitespace or invisible character")
            } else if !valid_walk_anchor(character) || character.unicode.is_none() {
                class_f += 1;
                (
                    "F",
                    "walk character has no classifiable geometry or Unicode",
                )
            } else if matches.walk_candidate_counts[index] > 0 {
                class_f += 1;
                ("F", "walk residual has a non-unique geometry candidate")
            } else {
                class_d += 1;
                ("D", "walk-only visible non-whitespace character")
            };
        push_residual(
            &mut residuals,
            &mut residual_count,
            detail_limit,
            Residual {
                class,
                reason,
                match_stage: None,
                walk: Some(walk_character(index, character)),
                engine: None,
                delta_x_pt: None,
                delta_y_pt: None,
            },
        );
    }

    for &index in &matches.unmatched_engine {
        let character = &engine[index];
        let (class, reason) = if !engine_character_intersects_page(character, page_geometry) {
            class_a += 1;
            class_a_engine_outside_page += 1;
            ("A", "engine-only character is outside the visible page")
        } else if character.unicode.is_some_and(char::is_whitespace) {
            class_a += 1;
            ("A", "engine-only whitespace character")
        } else if !valid_engine_anchor(character) || character.unicode.is_none() {
            class_f += 1;
            (
                "F",
                "engine character has no classifiable geometry or Unicode",
            )
        } else if matches.engine_candidate_counts[index] > 0 {
            class_f += 1;
            ("F", "engine residual has a non-unique geometry candidate")
        } else {
            class_e += 1;
            ("E", "engine-only visible non-whitespace character")
        };
        push_residual(
            &mut residuals,
            &mut residual_count,
            detail_limit,
            Residual {
                class,
                reason,
                match_stage: None,
                walk: None,
                engine: Some(engine_character(index, character)),
                delta_x_pt: None,
                delta_y_pt: None,
            },
        );
    }

    let (class_b_moved_pairs, class_b_inversions, moved_pairs) = order_divergence(&order_pairs);
    for pair in moved_pairs {
        push_residual(
            &mut residuals,
            &mut residual_count,
            detail_limit,
            residual_pair(
                "B",
                "geometry matched but relative array order moved",
                &pair,
                walked,
                engine,
            ),
        );
    }

    PageClassification {
        page_index,
        tolerance_pt: tolerance,
        walked_characters: walked.len(),
        engine_characters: engine.len(),
        geometrically_matched: matches.pairs.len(),
        exact_matches: matches
            .pairs
            .iter()
            .filter(|pair| pair.stage == MatchStage::Exact)
            .count(),
        reconciled_same_unicode: matches
            .pairs
            .iter()
            .filter(|pair| pair.stage == MatchStage::ReconciledSameUnicode)
            .count(),
        reconciled_unique_mismatch: matches
            .pairs
            .iter()
            .filter(|pair| pair.stage == MatchStage::ReconciledUniqueMismatch)
            .count(),
        sequence_only_correspondences: matches.sequence_only_correspondences,
        same_unicode,
        class_a,
        class_a_engine_outside_page,
        class_b_moved_pairs,
        class_b_inversions,
        class_c,
        class_c_pdfium_hyphen,
        class_c_pdfium_utf16_surrogate,
        class_c_pdfium_ligature_expansion,
        class_c_strong_other,
        class_c_weak_other,
        class_c_unresolved,
        class_d,
        class_e,
        class_f,
        ambiguous_walk_nodes: matches
            .walk_candidate_counts
            .iter()
            .filter(|&&count| count > 1)
            .count(),
        ambiguous_engine_nodes: matches
            .engine_candidate_counts
            .iter()
            .filter(|&&count| count > 1)
            .count(),
        max_delta_x_pt,
        max_delta_y_pt,
        recoveries: recoveries.to_vec(),
        error: None,
        residuals_truncated: residual_count > residuals.len(),
        residuals,
    }
}

fn match_staged(walked: &[WalkedChar], engine: &[PageCharSnapshot], tolerance: f64) -> MatchResult {
    let mut walk_available = vec![true; walked.len()];
    let mut engine_available = vec![true; engine.len()];
    let exact_tolerance = tolerance.min(EXACT_TOLERANCE_PT);
    let exact_candidates = build_candidate_graph(
        walked,
        engine,
        exact_tolerance,
        &walk_available,
        &engine_available,
        CandidateKind::Any,
        false,
    );
    let mut pairs = greedy_one_to_one(walked, engine, &exact_candidates, MatchStage::Exact);
    mark_unavailable(&pairs, &mut walk_available, &mut engine_available);

    let mut walk_candidate_counts = exact_candidates.iter().map(Vec::len).collect::<Vec<_>>();
    let mut engine_candidate_counts = count_engine_candidates(&exact_candidates, engine.len());

    if tolerance > EXACT_TOLERANCE_PT {
        let broad_candidates = build_candidate_graph(
            walked,
            engine,
            tolerance,
            &walk_available,
            &engine_available,
            CandidateKind::Any,
            true,
        );
        for (count, broad) in walk_candidate_counts
            .iter_mut()
            .zip(broad_candidates.iter().map(Vec::len))
        {
            *count = (*count).max(broad);
        }
        for (count, broad) in engine_candidate_counts
            .iter_mut()
            .zip(count_engine_candidates(&broad_candidates, engine.len()))
        {
            *count = (*count).max(broad);
        }

        let equivalent_candidates = build_candidate_graph(
            walked,
            engine,
            tolerance,
            &walk_available,
            &engine_available,
            CandidateKind::EquivalentUnicode,
            true,
        );
        let equivalent_pairs = greedy_one_to_one(
            walked,
            engine,
            &equivalent_candidates,
            MatchStage::ReconciledSameUnicode,
        );
        mark_unavailable(
            &equivalent_pairs,
            &mut walk_available,
            &mut engine_available,
        );
        pairs.extend(equivalent_pairs);

        loop {
            let mismatch_candidates = build_candidate_graph(
                walked,
                engine,
                tolerance,
                &walk_available,
                &engine_available,
                CandidateKind::DifferentUnicode,
                true,
            );
            let mismatch_engine_counts =
                count_engine_candidates(&mismatch_candidates, engine.len());
            let unique_pairs = mismatch_candidates
                .iter()
                .enumerate()
                .filter_map(|(walk_index, candidates)| {
                    let &(engine_index, _) = candidates.as_slice().first()?;
                    (candidates.len() == 1
                        && mismatch_engine_counts[engine_index] == 1
                        && sequence_supported(
                            walk_index,
                            engine_index,
                            &pairs,
                            walked,
                            engine,
                            tolerance,
                        ))
                    .then_some(Pair {
                        walk: walk_index,
                        engine: engine_index,
                        delta_x: (walked[walk_index].baseline_origin.x
                            - engine[engine_index].baseline_origin.x)
                            .abs(),
                        delta_y: (walked[walk_index].baseline_origin.y
                            - engine[engine_index].baseline_origin.y)
                            .abs(),
                        stage: MatchStage::ReconciledUniqueMismatch,
                    })
                })
                .collect::<Vec<_>>();
            if unique_pairs.is_empty() {
                break;
            }
            mark_unavailable(&unique_pairs, &mut walk_available, &mut engine_available);
            pairs.extend(unique_pairs);
        }
    }

    pairs.sort_by_key(|pair| pair.walk);
    let sequence_only_correspondences = mark_sequence_only_correspondences(
        walked,
        engine,
        &pairs,
        &walk_available,
        &engine_available,
        &mut walk_candidate_counts,
        &mut engine_candidate_counts,
    );
    let unmatched_walk = walk_available
        .iter()
        .enumerate()
        .filter_map(|(index, &available)| available.then_some(index))
        .collect();
    let unmatched_engine = engine_available
        .iter()
        .enumerate()
        .filter_map(|(index, &available)| available.then_some(index))
        .collect();
    MatchResult {
        pairs,
        unmatched_walk,
        unmatched_engine,
        walk_candidate_counts,
        engine_candidate_counts,
        sequence_only_correspondences,
    }
}

#[derive(Debug, Clone, Copy)]
enum CandidateKind {
    Any,
    EquivalentUnicode,
    DifferentUnicode,
}

fn build_candidate_graph(
    walked: &[WalkedChar],
    engine: &[PageCharSnapshot],
    tolerance: f64,
    walk_available: &[bool],
    engine_available: &[bool],
    kind: CandidateKind,
    require_line_and_box_support: bool,
) -> Vec<Vec<(usize, f64)>> {
    let mut buckets: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (index, character) in engine.iter().enumerate() {
        if engine_available[index] && valid_engine_anchor(character) {
            buckets
                .entry(grid_key(
                    character.baseline_origin.x,
                    character.baseline_origin.y,
                    tolerance,
                ))
                .or_default()
                .push(index);
        }
    }

    let mut candidates = vec![Vec::<(usize, f64)>::new(); walked.len()];
    for (walk_index, character) in walked.iter().enumerate() {
        if !walk_available[walk_index] || !valid_walk_anchor(character) {
            continue;
        }
        let (grid_x, grid_y) = grid_key(
            character.baseline_origin.x,
            character.baseline_origin.y,
            tolerance,
        );
        for offset_x in -1..=1 {
            for offset_y in -1..=1 {
                if let Some(indices) = buckets.get(&(grid_x + offset_x, grid_y + offset_y)) {
                    for &engine_index in indices {
                        let engine_character = &engine[engine_index];
                        let delta_x = (character.baseline_origin.x
                            - engine_character.baseline_origin.x)
                            .abs();
                        let delta_y = (character.baseline_origin.y
                            - engine_character.baseline_origin.y)
                            .abs();
                        let unicode_equivalent = unicode_equivalent(character, engine_character);
                        let kind_matches = match kind {
                            CandidateKind::Any => true,
                            CandidateKind::EquivalentUnicode => unicode_equivalent,
                            CandidateKind::DifferentUnicode => !unicode_equivalent,
                        };
                        if delta_x <= tolerance
                            && delta_y <= tolerance
                            && kind_matches
                            && (!require_line_and_box_support
                                || line_and_box_support(character, engine_character))
                        {
                            let distance = delta_x.mul_add(delta_x, delta_y * delta_y);
                            candidates[walk_index].push((engine_index, distance));
                        }
                    }
                }
            }
        }
        candidates[walk_index].sort_by(
            |(left_index, left_distance), (right_index, right_distance)| {
                let left_same = walked[walk_index].unicode == engine[*left_index].unicode;
                let right_same = walked[walk_index].unicode == engine[*right_index].unicode;
                right_same
                    .cmp(&left_same)
                    .then_with(|| left_distance.total_cmp(right_distance))
                    .then_with(|| left_index.cmp(right_index))
            },
        );
    }

    candidates
}

fn greedy_one_to_one(
    walked: &[WalkedChar],
    engine: &[PageCharSnapshot],
    candidates: &[Vec<(usize, f64)>],
    stage: MatchStage,
) -> Vec<Pair> {
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
            let left_equivalent = unicode_equivalent(&walked[*left_walk], &engine[*left_engine]);
            let right_equivalent = unicode_equivalent(&walked[*right_walk], &engine[*right_engine]);
            right_equivalent
                .cmp(&left_equivalent)
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
        pairs.push(Pair {
            walk: walk_index,
            engine: engine_index,
            delta_x: (walked[walk_index].baseline_origin.x
                - engine[engine_index].baseline_origin.x)
                .abs(),
            delta_y: (walked[walk_index].baseline_origin.y
                - engine[engine_index].baseline_origin.y)
                .abs(),
            stage,
        });
    }
    pairs
}

fn count_engine_candidates(candidates: &[Vec<(usize, f64)>], engine_len: usize) -> Vec<usize> {
    let mut counts = vec![0usize; engine_len];
    for engine_index in candidates
        .iter()
        .flat_map(|candidate_list| candidate_list.iter().map(|(index, _)| *index))
    {
        counts[engine_index] += 1;
    }
    counts
}

fn mark_unavailable(pairs: &[Pair], walk_available: &mut [bool], engine_available: &mut [bool]) {
    for pair in pairs {
        walk_available[pair.walk] = false;
        engine_available[pair.engine] = false;
    }
}

fn grid_key(x: f64, y: f64, tolerance: f64) -> (i64, i64) {
    (
        (x / tolerance).floor() as i64,
        (y / tolerance).floor() as i64,
    )
}

fn valid_walk_anchor(character: &WalkedChar) -> bool {
    character.locatable
        && character.text_transform == TextTransform::Upright
        && character.baseline_origin.x.is_finite()
        && character.baseline_origin.y.is_finite()
}

fn mark_sequence_only_correspondences(
    walked: &[WalkedChar],
    engine: &[PageCharSnapshot],
    anchors: &[Pair],
    walk_available: &[bool],
    engine_available: &[bool],
    walk_candidate_counts: &mut [usize],
    engine_candidate_counts: &mut [usize],
) -> usize {
    let mut sequence_walk = vec![false; walked.len()];
    let mut sequence_engine = vec![false; engine.len()];
    let mut run_start = 0usize;
    while run_start < walked.len() {
        if !walk_available[run_start] {
            run_start += 1;
            continue;
        }
        let mut run_end = run_start + 1;
        while run_end < walked.len() && walk_available[run_end] {
            run_end += 1;
        }

        let previous_engine = anchors
            .iter()
            .filter(|anchor| anchor.walk < run_start)
            .max_by_key(|anchor| anchor.walk)
            .map_or(0, |anchor| anchor.engine.saturating_add(1));
        let next_engine = anchors
            .iter()
            .filter(|anchor| anchor.walk >= run_end)
            .min_by_key(|anchor| anchor.walk)
            .map_or(engine.len(), |anchor| anchor.engine);
        if previous_engine <= next_engine {
            let proposed = greedy_sequence_correspondences(
                run_start..run_end,
                previous_engine..next_engine,
                walked,
                engine,
                engine_available,
            );
            let visible_walk = walked[run_start..run_end]
                .iter()
                .filter(|character| {
                    character.visible
                        && character.unicode.is_some()
                        && !character.unicode.is_some_and(char::is_whitespace)
                })
                .count();
            if proposed.len() >= 2 && proposed.len() * 4 >= visible_walk * 3 {
                for (walk_index, engine_index) in proposed {
                    if !sequence_walk[walk_index] && !sequence_engine[engine_index] {
                        sequence_walk[walk_index] = true;
                        sequence_engine[engine_index] = true;
                    }
                }
            }
        }
        run_start = run_end;
    }
    for walk_index in 0..walked.len() {
        if walk_available[walk_index]
            && !sequence_walk[walk_index]
            && engine_available.get(walk_index).copied().unwrap_or(false)
            && !sequence_engine[walk_index]
            && unicode_equivalent(&walked[walk_index], &engine[walk_index])
        {
            sequence_walk[walk_index] = true;
            sequence_engine[walk_index] = true;
        }
    }
    let mut total = 0usize;
    for (index, marked) in sequence_walk.into_iter().enumerate() {
        if marked {
            walk_candidate_counts[index] = walk_candidate_counts[index].saturating_add(1);
            total += 1;
        }
    }
    for (index, marked) in sequence_engine.into_iter().enumerate() {
        if marked {
            engine_candidate_counts[index] = engine_candidate_counts[index].saturating_add(1);
        }
    }
    total
}

fn greedy_sequence_correspondences(
    walk_range: std::ops::Range<usize>,
    engine_range: std::ops::Range<usize>,
    walked: &[WalkedChar],
    engine: &[PageCharSnapshot],
    engine_available: &[bool],
) -> Vec<(usize, usize)> {
    const LOOKAHEAD: usize = 16;
    let available_engine = engine_range
        .filter(|&index| {
            engine_available[index]
                && engine[index].unicode.is_some()
                && !engine[index].unicode.is_some_and(char::is_whitespace)
        })
        .collect::<Vec<_>>();
    let mut cursor = 0usize;
    let mut proposed = Vec::new();
    for walk_index in walk_range.filter(|&index| {
        walked[index].visible
            && walked[index].unicode.is_some()
            && !walked[index].unicode.is_some_and(char::is_whitespace)
    }) {
        let Some(relative) =
            available_engine[cursor..]
                .iter()
                .take(LOOKAHEAD)
                .position(|&engine_index| {
                    unicode_equivalent(&walked[walk_index], &engine[engine_index])
                })
        else {
            continue;
        };
        cursor += relative;
        proposed.push((walk_index, available_engine[cursor]));
        cursor += 1;
        if cursor == available_engine.len() {
            break;
        }
    }
    proposed
}

fn valid_engine_anchor(character: &PageCharSnapshot) -> bool {
    character.baseline_origin.x.is_finite() && character.baseline_origin.y.is_finite()
}

fn engine_character_intersects_page(character: &PageCharSnapshot, geometry: PageGeometry) -> bool {
    character.tight_box.right > 0.0
        && character.tight_box.left < geometry.width
        && character.tight_box.top > 0.0
        && character.tight_box.bottom < geometry.height
}

fn unicode_equivalent(walk: &WalkedChar, engine: &PageCharSnapshot) -> bool {
    walk.unicode == engine.unicode || is_pdfium_hyphen_marker(walk, engine)
}

fn is_pdfium_hyphen_marker(walk: &WalkedChar, engine: &PageCharSnapshot) -> bool {
    engine.is_hyphen == Some(true)
        && engine.unicode_value == 2
        && walk
            .unicode
            .is_some_and(|value| matches!(value, '-' | '\u{00AD}' | '\u{2010}' | '\u{2011}'))
}

fn is_pdfium_utf16_surrogate(walk: &WalkedChar, engine: &PageCharSnapshot) -> bool {
    let Some(unicode) = walk.unicode else {
        return false;
    };
    let mut units = [0u16; 2];
    let encoded = unicode.encode_utf16(&mut units);
    encoded.len() == 2 && u32::from(encoded[0]) == engine.unicode_value
}

fn is_pdfium_ligature_expansion(walk: &WalkedChar, engine: &PageCharSnapshot) -> bool {
    matches!(
        (walk.unicode, engine.unicode),
        (
            Some('\u{FB00}' | '\u{FB01}' | '\u{FB02}' | '\u{FB03}' | '\u{FB04}'),
            Some('f')
        ) | (Some('\u{FB05}' | '\u{FB06}'), Some('s'))
    )
}

fn line_and_box_support(walk: &WalkedChar, engine: &PageCharSnapshot) -> bool {
    if walk.unicode.is_some_and(char::is_whitespace)
        || engine.unicode.is_some_and(char::is_whitespace)
        || !walk.visible
    {
        return true;
    }
    let walk_height = walk.metric_box.top - walk.metric_box.bottom;
    let engine_height = engine.loose_box.top - engine.loose_box.bottom;
    if !walk_height.is_finite()
        || !engine_height.is_finite()
        || walk_height <= 0.0
        || engine_height <= 0.0
    {
        return true;
    }
    walk.metric_box.top.min(engine.loose_box.top)
        > walk.metric_box.bottom.max(engine.loose_box.bottom)
}

fn sequence_supported(
    walk_index: usize,
    engine_index: usize,
    anchors: &[Pair],
    walked: &[WalkedChar],
    engine: &[PageCharSnapshot],
    line_tolerance: f64,
) -> bool {
    let walk_target = &walked[walk_index];
    let engine_target = &engine[engine_index];
    anchors.iter().any(|anchor| {
        let walk_anchor = &walked[anchor.walk];
        let engine_anchor = &engine[anchor.engine];
        let walk_dx = walk_anchor.baseline_origin.x - walk_target.baseline_origin.x;
        let engine_dx = engine_anchor.baseline_origin.x - engine_target.baseline_origin.x;
        (walk_anchor.baseline_origin.y - walk_target.baseline_origin.y).abs() <= line_tolerance
            && (engine_anchor.baseline_origin.y - engine_target.baseline_origin.y).abs()
                <= line_tolerance
            && walk_dx.abs() <= SEQUENCE_ANCHOR_DISTANCE_PT
            && engine_dx.abs() <= SEQUENCE_ANCHOR_DISTANCE_PT
            && walk_dx.signum() == engine_dx.signum()
    })
}

fn order_divergence(pairs: &[Pair]) -> (usize, u64, Vec<Pair>) {
    let mut by_walk = pairs.to_vec();
    by_walk.sort_by_key(|pair| pair.walk);
    let mut by_engine = pairs.to_vec();
    by_engine.sort_by_key(|pair| pair.engine);
    let engine_positions = by_engine
        .iter()
        .enumerate()
        .map(|(position, pair)| ((pair.walk, pair.engine), position))
        .collect::<HashMap<_, _>>();
    let moved = by_walk
        .iter()
        .enumerate()
        .filter_map(|(position, pair)| {
            (engine_positions[&(pair.walk, pair.engine)] != position).then_some(*pair)
        })
        .collect::<Vec<_>>();
    let mut fenwick = Fenwick::new(by_engine.len());
    let mut inversions = 0u64;
    for pair in by_walk.iter().rev() {
        let position = engine_positions[&(pair.walk, pair.engine)];
        inversions += fenwick.sum(position) as u64;
        fenwick.add(position, 1);
    }
    (moved.len(), inversions, moved)
}

struct Fenwick {
    tree: Vec<usize>,
}

impl Fenwick {
    fn new(length: usize) -> Self {
        Self {
            tree: vec![0; length + 1],
        }
    }

    fn add(&mut self, index: usize, value: usize) {
        let mut index = index + 1;
        while index < self.tree.len() {
            self.tree[index] += value;
            index += index & index.wrapping_neg();
        }
    }

    fn sum(&self, end_exclusive: usize) -> usize {
        let mut total = 0;
        let mut index = end_exclusive;
        while index > 0 {
            total += self.tree[index];
            index &= index - 1;
        }
        total
    }
}

fn residual_pair(
    class: &'static str,
    reason: &'static str,
    pair: &Pair,
    walked: &[WalkedChar],
    engine: &[PageCharSnapshot],
) -> Residual {
    Residual {
        class,
        reason,
        match_stage: Some(match_stage_name(pair.stage)),
        walk: Some(walk_character(pair.walk, &walked[pair.walk])),
        engine: Some(engine_character(pair.engine, &engine[pair.engine])),
        delta_x_pt: Some(pair.delta_x),
        delta_y_pt: Some(pair.delta_y),
    }
}

fn push_residual(
    residuals: &mut Vec<Residual>,
    total: &mut usize,
    limit: usize,
    residual: Residual,
) {
    *total += 1;
    if residuals.len() < limit {
        residuals.push(residual);
    }
}

fn walk_character(index: usize, character: &WalkedChar) -> WalkCharacter {
    WalkCharacter {
        index,
        unicode: character.unicode.map(|value| value.to_string()),
        unicode_value: character.unicode.map(u32::from),
        unicode_provenance: provenance_name(character.unicode_provenance),
        code: character.code,
        encoded_hex: character
            .encoded
            .iter()
            .map(|value| format!("{value:02X}"))
            .collect(),
        visible: character.visible,
        locatable: character.locatable,
        font_supported: character.font_supported,
        engine_mismatch_tolerated: character.engine_mismatch_tolerated,
        text_transform: format!("{:?}", character.text_transform),
        font_resource: character.font.resource_name.clone(),
        font_object: character.font.object_number,
        font_generation: character.font.generation,
        baseline_x: character.baseline_origin.x,
        baseline_y: character.baseline_origin.y,
        metric_left: character.metric_box.left,
        metric_bottom: character.metric_box.bottom,
        metric_right: character.metric_box.right,
        metric_top: character.metric_box.top,
        content_object: character.content_object.0,
        content_generation: character.content_object.1,
        byte_start: character.byte_start,
        byte_end: character.byte_end,
    }
}

fn engine_character(index: usize, character: &PageCharSnapshot) -> EngineCharacter {
    EngineCharacter {
        array_index: index,
        pdfium_index: character.index,
        unicode: character.unicode.map(|value| value.to_string()),
        unicode_value: character.unicode_value,
        is_hyphen: character.is_hyphen,
        baseline_x: character.baseline_origin.x,
        baseline_y: character.baseline_origin.y,
        tight_left: character.tight_box.left,
        tight_bottom: character.tight_box.bottom,
        tight_right: character.tight_box.right,
        tight_top: character.tight_box.top,
    }
}

fn provenance_name(value: UnicodeProvenance) -> &'static str {
    match value {
        UnicodeProvenance::ToUnicode => "to_unicode",
        UnicodeProvenance::EmbeddedFontCmap => "embedded_font_cmap",
        UnicodeProvenance::SimpleEncoding => "simple_encoding",
        UnicodeProvenance::Unresolved => "unresolved",
    }
}

fn match_stage_name(value: MatchStage) -> &'static str {
    match value {
        MatchStage::Exact => "exact",
        MatchStage::ReconciledSameUnicode => "reconciled_same_unicode",
        MatchStage::ReconciledUniqueMismatch => "reconciled_unique_mismatch",
    }
}

impl ToleranceSummary {
    fn add_page(&mut self, page: &PageClassification) {
        self.pages += 1;
        self.walked_characters += page.walked_characters;
        self.engine_characters += page.engine_characters;
        self.geometrically_matched += page.geometrically_matched;
        self.exact_matches += page.exact_matches;
        self.reconciled_same_unicode += page.reconciled_same_unicode;
        self.reconciled_unique_mismatch += page.reconciled_unique_mismatch;
        self.sequence_only_correspondences += page.sequence_only_correspondences;
        self.same_unicode += page.same_unicode;
        self.class_a += page.class_a;
        self.class_a_engine_outside_page += page.class_a_engine_outside_page;
        self.class_b_moved_pairs += page.class_b_moved_pairs;
        self.class_b_inversions += page.class_b_inversions;
        self.class_c += page.class_c;
        self.class_c_pdfium_hyphen += page.class_c_pdfium_hyphen;
        self.class_c_pdfium_utf16_surrogate += page.class_c_pdfium_utf16_surrogate;
        self.class_c_pdfium_ligature_expansion += page.class_c_pdfium_ligature_expansion;
        self.class_c_strong_other += page.class_c_strong_other;
        self.class_c_weak_other += page.class_c_weak_other;
        self.class_c_unresolved += page.class_c_unresolved;
        self.class_d += page.class_d;
        self.class_e += page.class_e;
        self.class_f += page.class_f;
        self.ambiguous_walk_nodes += page.ambiguous_walk_nodes;
        self.ambiguous_engine_nodes += page.ambiguous_engine_nodes;
        self.max_delta_x_pt = self.max_delta_x_pt.max(page.max_delta_x_pt);
        self.max_delta_y_pt = self.max_delta_y_pt.max(page.max_delta_y_pt);
    }

    fn add_summary(&mut self, summary: &Self) {
        self.pages += summary.pages;
        self.walked_characters += summary.walked_characters;
        self.engine_characters += summary.engine_characters;
        self.geometrically_matched += summary.geometrically_matched;
        self.exact_matches += summary.exact_matches;
        self.reconciled_same_unicode += summary.reconciled_same_unicode;
        self.reconciled_unique_mismatch += summary.reconciled_unique_mismatch;
        self.sequence_only_correspondences += summary.sequence_only_correspondences;
        self.same_unicode += summary.same_unicode;
        self.class_a += summary.class_a;
        self.class_a_engine_outside_page += summary.class_a_engine_outside_page;
        self.class_b_moved_pairs += summary.class_b_moved_pairs;
        self.class_b_inversions += summary.class_b_inversions;
        self.class_c += summary.class_c;
        self.class_c_pdfium_hyphen += summary.class_c_pdfium_hyphen;
        self.class_c_pdfium_utf16_surrogate += summary.class_c_pdfium_utf16_surrogate;
        self.class_c_pdfium_ligature_expansion += summary.class_c_pdfium_ligature_expansion;
        self.class_c_strong_other += summary.class_c_strong_other;
        self.class_c_weak_other += summary.class_c_weak_other;
        self.class_c_unresolved += summary.class_c_unresolved;
        self.class_d += summary.class_d;
        self.class_e += summary.class_e;
        self.class_f += summary.class_f;
        self.ambiguous_walk_nodes += summary.ambiguous_walk_nodes;
        self.ambiguous_engine_nodes += summary.ambiguous_engine_nodes;
        self.max_delta_x_pt = self.max_delta_x_pt.max(summary.max_delta_x_pt);
        self.max_delta_y_pt = self.max_delta_y_pt.max(summary.max_delta_y_pt);
    }
}

fn aggregate_document_summaries(
    documents: &[DocumentReport],
    tolerances: &[f64],
) -> Vec<ToleranceSummary> {
    tolerances
        .iter()
        .enumerate()
        .map(|(index, &tolerance_pt)| {
            let mut total = ToleranceSummary {
                tolerance_pt,
                ..ToleranceSummary::default()
            };
            for document in documents {
                if let Some(summary) = document.totals_by_tolerance.get(index) {
                    total.add_summary(summary);
                }
            }
            total
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerance_parser_preserves_primary_order_and_deduplicates() {
        assert_eq!(
            parse_tolerances("0.5,0.1,0.5,1.0").unwrap(),
            vec![0.5, 0.1, 1.0]
        );
    }

    #[test]
    fn tolerance_parser_rejects_non_positive_values() {
        assert!(parse_tolerances("0.5,0").is_err());
        assert!(parse_tolerances("-1").is_err());
    }
}
