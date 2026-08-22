use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use experiment_4_common::{
    atomic_write_json, content_sha256, error_class, micros_since, sha256_bytes, sha256_file,
    BatchJob, BatchReport, BatchRequest, CharacterReport, PageReport, PageTimings, Point, Rect,
    RenderReport, RunOutcome, RunReport, StageTimings, BACKEND_REVISION_PDFIUM_RENDER,
    SCHEMA_VERSION,
};
use pdfium_render::prelude::*;

#[derive(Debug, Parser)]
#[command(about = "Experiment 4 pdfium-render reference wrapper")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        pdf: PathBuf,
        #[arg(long)]
        input_id: String,
        #[arg(long)]
        pdfium_library: PathBuf,
        #[arg(long)]
        pdfium_release: String,
    },
    Batch {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        pdfium_library: PathBuf,
        #[arg(long)]
        pdfium_release: String,
        #[arg(long)]
        checkpoint: PathBuf,
        #[arg(long, default_value_t = 0)]
        warmup_rounds: u32,
        #[arg(long)]
        warmup_complete_marker: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        iterations: u32,
    },
}

fn main() -> ExitCode {
    match run_main() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("reference-runner protocol failure: {error:#}");
            ExitCode::from(3)
        }
    }
}

fn run_main() -> Result<u8> {
    match Args::parse().command {
        Command::Run {
            pdf,
            input_id,
            pdfium_library,
            pdfium_release,
        } => {
            let report = run_one(&pdf, &input_id, &pdfium_library, &pdfium_release);
            println!("{}", serde_json::to_string(&report)?);
            Ok(report.process_exit as u8)
        }
        Command::Batch {
            request,
            pdfium_library,
            pdfium_release,
            checkpoint,
            warmup_rounds,
            warmup_complete_marker,
            iterations,
        } => {
            let report = run_batch(
                &request,
                &pdfium_library,
                &pdfium_release,
                &checkpoint,
                warmup_rounds,
                warmup_complete_marker.as_deref(),
                iterations,
            )?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(if report.jobs.iter().all(|job| job.success) {
                0
            } else {
                2
            })
        }
    }
}

fn run_one(pdf_path: &Path, input_id: &str, library_path: &Path, release: &str) -> RunReport {
    let total_start = Instant::now();
    let library_sha = sha256_file(library_path).unwrap_or_default();
    let pdf_bytes = match fs::read(pdf_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return RunReport::error(
                "reference",
                BACKEND_REVISION_PDFIUM_RENDER,
                release,
                library_sha,
                input_id.to_owned(),
                String::new(),
                StageTimings {
                    total_us: micros_since(total_start),
                    ..StageTimings::default()
                },
                "input_io",
                error.to_string(),
            );
        }
    };
    let pdf_sha = sha256_bytes(&pdf_bytes);
    let mut timings = StageTimings::default();

    let library_start = Instant::now();
    let pdfium = match Pdfium::bind_to_library(library_path) {
        Ok(bindings) => Pdfium::new(bindings),
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Pdfium::default(),
        Err(error) => {
            timings.library_load_us = micros_since(library_start);
            timings.total_us = micros_since(total_start);
            return RunReport::error(
                "reference",
                BACKEND_REVISION_PDFIUM_RENDER,
                release,
                library_sha,
                input_id.to_owned(),
                pdf_sha,
                timings,
                "pdfium_library",
                error.to_string(),
            );
        }
    };
    timings.library_load_us = micros_since(library_start);

    let document_start = Instant::now();
    let document = match pdfium.load_pdf_from_byte_vec(pdf_bytes, None) {
        Ok(document) => document,
        Err(error) => {
            timings.document_load_us = micros_since(document_start);
            timings.total_us = micros_since(total_start);
            let error = anyhow::Error::new(error);
            return RunReport::error(
                "reference",
                BACKEND_REVISION_PDFIUM_RENDER,
                release,
                library_sha,
                input_id.to_owned(),
                pdf_sha,
                timings,
                &error_class(&error),
                format!("{error:#}"),
            );
        }
    };
    timings.document_load_us = micros_since(document_start);

    match extract_document(&document, &mut timings) {
        Ok(pages) => {
            timings.total_us = micros_since(total_start);
            RunReport {
                schema_version: SCHEMA_VERSION,
                backend: "reference".to_owned(),
                backend_revision: BACKEND_REVISION_PDFIUM_RENDER.to_owned(),
                pdfium_release: release.to_owned(),
                pdfium_library_sha256: library_sha,
                input_id: input_id.to_owned(),
                pdf_sha256: pdf_sha,
                pages,
                timings,
                outcome: RunOutcome::Success,
                process_exit: 0,
            }
        }
        Err(error) => {
            timings.total_us = micros_since(total_start);
            RunReport::error(
                "reference",
                BACKEND_REVISION_PDFIUM_RENDER,
                release,
                library_sha,
                input_id.to_owned(),
                pdf_sha,
                timings,
                &error_class(&error),
                format!("{error:#}"),
            )
        }
    }
}

fn extract_document(
    document: &PdfDocument<'_>,
    timings: &mut StageTimings,
) -> Result<Vec<PageReport>> {
    let mut pages = Vec::with_capacity(document.pages().len() as usize);
    for (page_index, page) in document.pages().iter().enumerate() {
        let text_start = Instant::now();
        let text_page = page
            .text()
            .with_context(|| format!("text page {page_index}"))?;
        let mut characters = Vec::with_capacity(text_page.len().max(0) as usize);
        let chars = text_page.chars();
        for character in chars.iter() {
            let tight = character.tight_bounds().context("tight character box")?;
            let loose = character.loose_bounds().context("loose character box")?;
            let (x, y) = character.origin().context("character origin")?;
            characters.push(CharacterReport {
                index: character.index() as u32,
                unicode: character.unicode_char().map(|value| value.to_string()),
                code: character.unicode_value(),
                origin: Point {
                    x: f64::from(x.value),
                    y: f64::from(y.value),
                },
                tight_box: rect(tight),
                loose_box: rect(loose),
            });
        }
        let text = text_from_character_codes(&characters);
        let text_us = micros_since(text_start);
        timings.text_us = timings.text_us.saturating_add(text_us);

        let width = page.width().value;
        let height = page.height().value;
        let width_px = dimension(width)?;
        let height_px = dimension(height)?;
        let render_start = Instant::now();
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_target_size(width_px as i32, height_px as i32)
                    .set_format(PdfBitmapFormat::BGRA)
                    .set_clear_color(PdfColor::WHITE)
                    .render_annotations(false)
                    .render_form_data(false),
            )
            .with_context(|| format!("render page {page_index}"))?;
        let rgba = bitmap.as_rgba_bytes();
        let render_us = micros_since(render_start);
        timings.render_us = timings.render_us.saturating_add(render_us);
        let rotation = page.rotation().context("page rotation")?;
        pages.push(PageReport {
            page_number: page_index as u32 + 1,
            width_pt: f64::from(width),
            height_pt: f64::from(height),
            rotate_degrees: rotation.as_degrees() as i32,
            text,
            characters,
            render: RenderReport {
                width_px: bitmap.width() as u32,
                height_px: bitmap.height() as u32,
                rgba8_sha256: sha256_bytes(&rgba),
            },
            timings: PageTimings { text_us, render_us },
        });
    }
    Ok(pages)
}

fn rect(value: PdfRect) -> Rect {
    Rect {
        left: f64::from(value.left().value),
        bottom: f64::from(value.bottom().value),
        right: f64::from(value.right().value),
        top: f64::from(value.top().value),
    }
}

fn text_from_character_codes(characters: &[CharacterReport]) -> String {
    let units = characters
        .iter()
        .flat_map(|character| {
            if let Ok(unit) = u16::try_from(character.code) {
                vec![unit]
            } else if let Some(value) = char::from_u32(character.code) {
                let mut encoded = [0; 2];
                value.encode_utf16(&mut encoded).to_vec()
            } else {
                vec![char::REPLACEMENT_CHARACTER as u16]
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&units)
}

fn dimension(points: f32) -> Result<u32> {
    let rounded = points.round();
    if !rounded.is_finite() || rounded <= 0.0 || rounded > i32::MAX as f32 {
        anyhow::bail!("invalid page dimension {points}");
    }
    Ok(rounded as u32)
}

fn run_batch(
    request_path: &Path,
    library_path: &Path,
    release: &str,
    checkpoint_path: &Path,
    warmup_rounds: u32,
    warmup_complete_marker: Option<&Path>,
    iterations: u32,
) -> Result<BatchReport> {
    anyhow::ensure!(iterations > 0, "iterations must be at least one");
    let request: BatchRequest = serde_json::from_slice(
        &fs::read(request_path).with_context(|| format!("read {}", request_path.display()))?,
    )?;
    anyhow::ensure!(
        request.schema_version == SCHEMA_VERSION,
        "unsupported batch schema"
    );
    for _ in 0..warmup_rounds {
        for input in &request.inputs {
            let report = run_one(
                Path::new(&input.path),
                &input.input_id,
                library_path,
                release,
            );
            anyhow::ensure!(
                matches!(report.outcome, RunOutcome::Success),
                "warm-up failed for {}",
                input.input_id
            );
        }
    }
    if let Some(path) = warmup_complete_marker {
        atomic_write_json(
            path,
            &serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "warmup_rounds": warmup_rounds,
            }),
        )?;
    }
    let batch_start = Instant::now();
    let mut completed = load_completed(checkpoint_path, release)?;
    for iteration in 1..=iterations {
        for input in &request.inputs {
            let job_id = batch_job_id(iteration, iterations, &input.input_id);
            if completed.contains_key(&job_id) {
                continue;
            }
            let start = Instant::now();
            let report = run_one(Path::new(&input.path), &job_id, library_path, release);
            let job = BatchJob {
                input_id: job_id.clone(),
                report_sha256: if matches!(report.outcome, RunOutcome::Success) {
                    Some(content_sha256(&report)?)
                } else {
                    None
                },
                elapsed_us: micros_since(start),
                page_count: report.pages.len(),
                success: matches!(report.outcome, RunOutcome::Success),
                error_class: match report.outcome {
                    RunOutcome::Error { class, .. } => Some(class),
                    RunOutcome::Success => None,
                },
            };
            completed.insert(job_id, job);
            write_batch_checkpoint(
                checkpoint_path,
                release,
                micros_since(batch_start),
                &completed,
            )?;
        }
    }
    let report = batch_report(release, micros_since(batch_start), completed);
    atomic_write_json(checkpoint_path, &report)?;
    Ok(report)
}

fn batch_job_id(iteration: u32, iterations: u32, input_id: &str) -> String {
    if iterations == 1 {
        input_id.to_owned()
    } else {
        format!("round-{iteration:03}:{input_id}")
    }
}

fn load_completed(path: &Path, release: &str) -> Result<BTreeMap<String, BatchJob>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let report: BatchReport = serde_json::from_slice(&fs::read(path)?)?;
    anyhow::ensure!(
        report.schema_version == SCHEMA_VERSION,
        "checkpoint schema mismatch"
    );
    anyhow::ensure!(
        report.pdfium_release == release,
        "checkpoint release mismatch"
    );
    Ok(report
        .jobs
        .into_iter()
        .map(|job| (job.input_id.clone(), job))
        .collect())
}

fn write_batch_checkpoint(
    path: &Path,
    release: &str,
    elapsed_us: u64,
    jobs: &BTreeMap<String, BatchJob>,
) -> Result<()> {
    atomic_write_json(path, &batch_report(release, elapsed_us, jobs.clone()))
}

fn batch_report(release: &str, elapsed_us: u64, jobs: BTreeMap<String, BatchJob>) -> BatchReport {
    BatchReport {
        schema_version: SCHEMA_VERSION,
        backend: "reference".to_owned(),
        backend_revision: BACKEND_REVISION_PDFIUM_RENDER.to_owned(),
        pdfium_release: release.to_owned(),
        threads: 1,
        elapsed_us,
        jobs: jobs.into_values().collect(),
    }
}
