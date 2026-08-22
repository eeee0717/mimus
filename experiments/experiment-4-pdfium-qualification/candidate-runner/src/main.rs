use std::collections::{BTreeMap, VecDeque};
use std::ffi::{c_int, c_void};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use experiment_4_common::{
    atomic_write_json, content_sha256, error_class, micros_since, sha256_bytes, sha256_file,
    BatchInput, BatchJob, BatchReport, BatchRequest, CharacterReport, PageReport, PageTimings,
    Point, Rect, RenderReport, RunOutcome, RunReport, StageTimings, BACKEND_REVISION_FIRECRAWL,
    SCHEMA_VERSION,
};
use firecrawl_pdfium::{Color, Pdfium, PixelFormat, RenderConfig, Rotation};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(about = "Experiment 4 firecrawl-pdfium candidate wrapper")]
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
        #[arg(long, default_value_t = 1)]
        threads: usize,
        #[arg(long, default_value_t = 0)]
        warmup_rounds: u32,
        #[arg(long)]
        warmup_complete_marker: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        iterations: u32,
    },
    ProbeIsGenerated {
        #[arg(long)]
        pdf: PathBuf,
        #[arg(long)]
        pdfium_library: PathBuf,
        #[arg(long)]
        pdfium_release: String,
    },
}

fn main() -> ExitCode {
    match run_main() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("candidate-runner protocol failure: {error:#}");
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
            threads,
            warmup_rounds,
            warmup_complete_marker,
            iterations,
        } => {
            let report = run_batch(
                &request,
                &pdfium_library,
                &pdfium_release,
                &checkpoint,
                threads,
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
        Command::ProbeIsGenerated {
            pdf,
            pdfium_library,
            pdfium_release,
        } => {
            let report = raw_is_generated_probe(&pdf, &pdfium_library, &pdfium_release)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(if report.passed { 0 } else { 2 })
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
                "candidate",
                BACKEND_REVISION_FIRECRAWL,
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
    let pdfium = match Pdfium::load_from_path(library_path) {
        Ok(pdfium) => pdfium,
        Err(error) => {
            timings.library_load_us = micros_since(library_start);
            timings.total_us = micros_since(total_start);
            let error = anyhow::Error::new(error);
            return RunReport::error(
                "candidate",
                BACKEND_REVISION_FIRECRAWL,
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
    timings.library_load_us = micros_since(library_start);

    let document_start = Instant::now();
    let document = match pdfium.load_document(pdf_bytes, None) {
        Ok(document) => document,
        Err(error) => {
            timings.document_load_us = micros_since(document_start);
            timings.total_us = micros_since(total_start);
            let error = anyhow::Error::new(error);
            return RunReport::error(
                "candidate",
                BACKEND_REVISION_FIRECRAWL,
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
                backend: "candidate".to_owned(),
                backend_revision: BACKEND_REVISION_FIRECRAWL.to_owned(),
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
                "candidate",
                BACKEND_REVISION_FIRECRAWL,
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
    document: &firecrawl_pdfium::PdfDocument,
    timings: &mut StageTimings,
) -> Result<Vec<PageReport>> {
    let mut pages = Vec::with_capacity(document.page_count());
    for page_index in 0..document.page_count() {
        let page = document
            .page(page_index)
            .with_context(|| format!("load page {page_index}"))?;
        let text_start = Instant::now();
        let text_page = page
            .text()
            .with_context(|| format!("text page {page_index}"))?;
        let characters = text_page
            .chars()
            .iter()
            .enumerate()
            .map(|(index, character)| CharacterReport {
                index: index as u32,
                unicode: character.unicode.map(|value| value.to_string()),
                code: character.code,
                origin: Point {
                    x: character.origin.x,
                    y: character.origin.y,
                },
                tight_box: Rect {
                    left: character.bounds.left,
                    bottom: character.bounds.bottom,
                    right: character.bounds.right,
                    top: character.bounds.top,
                },
                loose_box: Rect {
                    left: character.loose_bounds.left,
                    bottom: character.loose_bounds.bottom,
                    right: character.loose_bounds.right,
                    top: character.loose_bounds.top,
                },
            })
            .collect();
        let text_us = micros_since(text_start);
        timings.text_us = timings.text_us.saturating_add(text_us);

        let render_start = Instant::now();
        let rendered = page
            .render(
                &RenderConfig::new()
                    .scale(1.0)
                    .pixel_format(PixelFormat::Bgra8)
                    .background(Color::WHITE)
                    .annotations(false)
                    .form_fields(false)
                    .text_antialiasing(true)
                    .image_antialiasing(true)
                    .path_antialiasing(true),
            )
            .with_context(|| format!("render page {page_index}"))?;
        let rgba = rendered.to_rgba8();
        let render_us = micros_since(render_start);
        timings.render_us = timings.render_us.saturating_add(render_us);
        pages.push(PageReport {
            page_number: page_index as u32 + 1,
            width_pt: f64::from(page.width()),
            height_pt: f64::from(page.height()),
            rotate_degrees: rotation_degrees(page.rotation()),
            text: text_page.text().to_owned(),
            characters,
            render: RenderReport {
                width_px: rendered.width(),
                height_px: rendered.height(),
                rgba8_sha256: sha256_bytes(&rgba),
            },
            timings: PageTimings { text_us, render_us },
        });
    }
    Ok(pages)
}

fn rotation_degrees(rotation: Rotation) -> i32 {
    match rotation {
        Rotation::None => 0,
        Rotation::Clockwise90 => 90,
        Rotation::Rotate180 => 180,
        Rotation::Clockwise270 => 270,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_batch(
    request_path: &Path,
    library_path: &Path,
    release: &str,
    checkpoint_path: &Path,
    threads: usize,
    warmup_rounds: u32,
    warmup_complete_marker: Option<&Path>,
    iterations: u32,
) -> Result<BatchReport> {
    anyhow::ensure!(threads > 0, "threads must be at least one");
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
    let completed = Arc::new(Mutex::new(load_completed(
        checkpoint_path,
        release,
        threads,
    )?));
    let pending = (1..=iterations)
        .flat_map(|iteration| {
            request.inputs.iter().map(move |input| BatchInput {
                input_id: batch_job_id(iteration, iterations, &input.input_id),
                path: input.path.clone(),
            })
        })
        .filter(|input| {
            !completed
                .lock()
                .expect("completed lock")
                .contains_key(&input.input_id)
        })
        .collect::<VecDeque<_>>();
    let pending = Arc::new(Mutex::new(pending));

    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let completed = Arc::clone(&completed);
            let pending = Arc::clone(&pending);
            handles.push(scope.spawn(move || -> Result<()> {
                loop {
                    let input = pending.lock().expect("pending lock").pop_front();
                    let Some(input) = input else { break };
                    let job = run_batch_job(&input, library_path, release)?;
                    let mut completed = completed.lock().expect("completed lock");
                    completed.insert(input.input_id, job);
                    write_batch_checkpoint(
                        checkpoint_path,
                        release,
                        threads,
                        micros_since(batch_start),
                        &completed,
                    )?;
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("batch worker panicked"))??;
        }
        Ok(())
    })?;

    let completed = Arc::try_unwrap(completed)
        .map_err(|_| anyhow::anyhow!("batch result still shared"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("batch result lock poisoned"))?;
    let report = batch_report(release, threads, micros_since(batch_start), completed);
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

fn run_batch_job(input: &BatchInput, library_path: &Path, release: &str) -> Result<BatchJob> {
    let start = Instant::now();
    let report = run_one(
        Path::new(&input.path),
        &input.input_id,
        library_path,
        release,
    );
    Ok(BatchJob {
        input_id: input.input_id.clone(),
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
    })
}

fn load_completed(
    path: &Path,
    release: &str,
    threads: usize,
) -> Result<BTreeMap<String, BatchJob>> {
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
    anyhow::ensure!(
        report.threads == threads,
        "checkpoint thread count mismatch"
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
    threads: usize,
    elapsed_us: u64,
    jobs: &BTreeMap<String, BatchJob>,
) -> Result<()> {
    atomic_write_json(
        path,
        &batch_report(release, threads, elapsed_us, jobs.clone()),
    )
}

fn batch_report(
    release: &str,
    threads: usize,
    elapsed_us: u64,
    jobs: BTreeMap<String, BatchJob>,
) -> BatchReport {
    BatchReport {
        schema_version: SCHEMA_VERSION,
        backend: "candidate".to_owned(),
        backend_revision: BACKEND_REVISION_FIRECRAWL.to_owned(),
        pdfium_release: release.to_owned(),
        threads,
        elapsed_us,
        jobs: jobs.into_values().collect(),
    }
}

#[derive(Debug, Serialize)]
struct RawProbeReport {
    schema_version: u32,
    pdfium_release: String,
    pdfium_library_sha256: String,
    pdf_sha256: String,
    char_count: i32,
    ordinary: Option<RawProbeCharacter>,
    generated_newline: Option<RawProbeCharacter>,
    out_of_range: i32,
    passed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RawProbeCharacter {
    index: i32,
    code: u32,
    is_generated_raw: i32,
}

fn raw_is_generated_probe(
    pdf_path: &Path,
    library_path: &Path,
    release: &str,
) -> Result<RawProbeReport> {
    type Handle = *mut c_void;
    type Init = unsafe extern "C" fn(*const firecrawl_pdfium::sys::FPDF_LIBRARY_CONFIG);
    type Destroy = unsafe extern "C" fn();
    type LoadDocument = unsafe extern "C" fn(*const c_void, usize, *const i8) -> Handle;
    type CloseDocument = unsafe extern "C" fn(Handle);
    type LoadPage = unsafe extern "C" fn(Handle, c_int) -> Handle;
    type ClosePage = unsafe extern "C" fn(Handle);
    type LoadTextPage = unsafe extern "C" fn(Handle) -> Handle;
    type CloseTextPage = unsafe extern "C" fn(Handle);
    type CountChars = unsafe extern "C" fn(Handle) -> c_int;
    type GetUnicode = unsafe extern "C" fn(Handle, c_int) -> u32;
    type IsGenerated = unsafe extern "C" fn(Handle, c_int) -> c_int;

    let bytes = fs::read(pdf_path).with_context(|| format!("read {}", pdf_path.display()))?;
    let library_sha = sha256_file(library_path)?;
    let library = unsafe { libloading::Library::new(library_path) }
        .with_context(|| format!("load {}", library_path.display()))?;
    unsafe {
        let init: libloading::Symbol<Init> = library.get(b"FPDF_InitLibraryWithConfig\0")?;
        let destroy: libloading::Symbol<Destroy> = library.get(b"FPDF_DestroyLibrary\0")?;
        let load_document: libloading::Symbol<LoadDocument> =
            library.get(b"FPDF_LoadMemDocument64\0")?;
        let close_document: libloading::Symbol<CloseDocument> =
            library.get(b"FPDF_CloseDocument\0")?;
        let load_page: libloading::Symbol<LoadPage> = library.get(b"FPDF_LoadPage\0")?;
        let close_page: libloading::Symbol<ClosePage> = library.get(b"FPDF_ClosePage\0")?;
        let load_text_page: libloading::Symbol<LoadTextPage> =
            library.get(b"FPDFText_LoadPage\0")?;
        let close_text_page: libloading::Symbol<CloseTextPage> =
            library.get(b"FPDFText_ClosePage\0")?;
        let count_chars: libloading::Symbol<CountChars> = library.get(b"FPDFText_CountChars\0")?;
        let get_unicode: libloading::Symbol<GetUnicode> = library.get(b"FPDFText_GetUnicode\0")?;
        let is_generated: libloading::Symbol<IsGenerated> =
            library.get(b"FPDFText_IsGenerated\0")?;

        let config = firecrawl_pdfium::sys::FPDF_LIBRARY_CONFIG {
            version: 2,
            m_pUserFontPaths: std::ptr::null(),
            m_pIsolate: std::ptr::null_mut(),
            m_v8EmbedderSlot: 0,
            m_pPlatform: std::ptr::null_mut(),
            m_RendererType: 0,
            m_FontLibraryType: 0,
            m_BrotliEnabled: 0,
        };
        init(&config);
        let document = load_document(bytes.as_ptr().cast(), bytes.len(), std::ptr::null());
        if document.is_null() {
            destroy();
            anyhow::bail!("FPDF_LoadMemDocument64 returned null");
        }
        let page = load_page(document, 0);
        if page.is_null() {
            close_document(document);
            destroy();
            anyhow::bail!("FPDF_LoadPage returned null");
        }
        let text_page = load_text_page(page);
        if text_page.is_null() {
            close_page(page);
            close_document(document);
            destroy();
            anyhow::bail!("FPDFText_LoadPage returned null");
        }
        let count = count_chars(text_page);
        let mut ordinary = None;
        let mut generated_newline = None;
        for index in 0..count {
            let code = get_unicode(text_page, index);
            let raw = is_generated(text_page, index);
            let observed = RawProbeCharacter {
                index,
                code,
                is_generated_raw: raw,
            };
            if raw == 0 && code != u32::from('\r') && code != u32::from('\n') {
                ordinary.get_or_insert_with(|| observed.clone());
            }
            if raw == 1 && (code == u32::from('\r') || code == u32::from('\n')) {
                generated_newline.get_or_insert(observed);
            }
        }
        let out_of_range = is_generated(text_page, count);
        close_text_page(text_page);
        close_page(page);
        close_document(document);
        destroy();
        let passed = ordinary.is_some() && generated_newline.is_some() && out_of_range == -1;
        Ok(RawProbeReport {
            schema_version: SCHEMA_VERSION,
            pdfium_release: release.to_owned(),
            pdfium_library_sha256: library_sha,
            pdf_sha256: sha256_bytes(&bytes),
            char_count: count,
            ordinary,
            generated_newline,
            out_of_range,
            passed,
        })
    }
}
