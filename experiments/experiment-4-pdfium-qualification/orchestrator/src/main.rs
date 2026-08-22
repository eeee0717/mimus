use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use experiment_4_common::{
    atomic_write_json, BatchInput, BatchReport, BatchRequest, ComparisonReport, ComparisonResult,
    Difference, RunOutcome, RunReport, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt;

const GEOMETRY_TOLERANCE_PT: f64 = 0.001;

#[derive(Debug, Parser)]
#[command(about = "Experiment 4 process orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: OrchestratorCommand,
}

#[derive(Debug, Subcommand)]
enum OrchestratorCommand {
    Matrix(MatrixArgs),
    Benchmark(BenchmarkArgs),
    LongRun(LongRunArgs),
}

#[derive(Debug, Clone, Args)]
struct RunnerArgs {
    #[arg(long)]
    reference_runner: PathBuf,
    #[arg(long)]
    candidate_runner: PathBuf,
    #[arg(long = "pdfium", value_parser = parse_pdfium)]
    pdfiums: Vec<PdfiumSpec>,
}

#[derive(Debug, Args)]
struct MatrixArgs {
    #[command(flatten)]
    runners: RunnerArgs,
    #[arg(long)]
    request: Option<PathBuf>,
    #[arg(long, default_value = "../..")]
    repo_root: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    is_generated_fixture: PathBuf,
    #[arg(long, default_value_t = 30)]
    timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct BenchmarkArgs {
    #[command(flatten)]
    runners: RunnerArgs,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "1,2,4,8")]
    thread_counts: String,
    #[arg(long, default_value = "1,2,4,8")]
    process_counts: String,
    #[arg(long, default_value_t = 1800)]
    timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct LongRunArgs {
    #[command(flatten)]
    runners: RunnerArgs,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 200)]
    max_rounds: u32,
    #[arg(long, default_value_t = 8.0)]
    max_hours: f64,
}

#[derive(Debug, Clone)]
struct PdfiumSpec {
    release: String,
    library: PathBuf,
}

fn parse_pdfium(value: &str) -> std::result::Result<PdfiumSpec, String> {
    let (release, library) = value
        .split_once('=')
        .ok_or_else(|| "expected RELEASE=/absolute/path/to/libpdfium".to_owned())?;
    if release.is_empty() || library.is_empty() {
        return Err("release and library path must be non-empty".to_owned());
    }
    Ok(PdfiumSpec {
        release: release.to_owned(),
        library: PathBuf::from(library),
    })
}

fn main() -> Result<()> {
    match Cli::parse().command {
        OrchestratorCommand::Matrix(args) => run_matrix(args),
        OrchestratorCommand::Benchmark(args) => run_benchmark(args),
        OrchestratorCommand::LongRun(args) => run_long_run(args),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MatrixSummary {
    schema_version: u32,
    comparisons: Vec<ComparisonReport>,
}

fn run_matrix(args: MatrixArgs) -> Result<()> {
    validate_runners(&args.runners)?;
    let request = match args.request {
        Some(path) => read_request(&path)?,
        None => discover_corpus(&args.repo_root)?,
    };
    fs::create_dir_all(&args.output)?;
    atomic_write_json(&args.output.join("request.json"), &request)?;
    atomic_write_json(
        &args.output.join("legal-request.json"),
        &BatchRequest {
            schema_version: SCHEMA_VERSION,
            inputs: request
                .inputs
                .iter()
                .filter(|input| !input.input_id.starts_with("mal-"))
                .cloned()
                .collect(),
        },
    )?;
    atomic_write_json(
        &args.output.join("malformed-request.json"),
        &BatchRequest {
            schema_version: SCHEMA_VERSION,
            inputs: request
                .inputs
                .iter()
                .filter(|input| input.input_id.starts_with("mal-"))
                .cloned()
                .collect(),
        },
    )?;
    let summary_path = args.output.join("matrix-checkpoint.json");
    let mut comparisons = load_matrix_checkpoint(&summary_path)?;
    let timeout = Duration::from_secs(args.timeout_seconds);

    for pdfium in &args.runners.pdfiums {
        for input in &request.inputs {
            if comparisons.iter().any(|item| {
                item.pdfium_release == pdfium.release && item.input_id == input.input_id
            }) {
                continue;
            }
            let item_dir = args
                .output
                .join("raw")
                .join(&pdfium.release)
                .join(&input.input_id);
            fs::create_dir_all(&item_dir)?;
            let reference = run_wrapper(&args.runners.reference_runner, input, pdfium, timeout)?;
            let candidate = run_wrapper(&args.runners.candidate_runner, input, pdfium, timeout)?;
            write_capture(&item_dir.join("reference.json"), &reference)?;
            write_capture(&item_dir.join("candidate.json"), &candidate)?;
            let comparison =
                compare_captures(&pdfium.release, &input.input_id, &reference, &candidate);
            atomic_write_json(&item_dir.join("comparison.json"), &comparison)?;
            comparisons.push(comparison);
            atomic_write_json(
                &summary_path,
                &MatrixSummary {
                    schema_version: SCHEMA_VERSION,
                    comparisons: comparisons.clone(),
                },
            )?;
        }
        run_probe(
            &args.runners.candidate_runner,
            pdfium,
            &args.is_generated_fixture,
            &args.output,
            timeout,
        )?;
    }

    let legal_failures = comparisons
        .iter()
        .filter(|comparison| !comparison.input_id.starts_with("mal-"))
        .filter(|comparison| comparison.result != ComparisonResult::Equal)
        .count();
    let unbounded_malformed = comparisons
        .iter()
        .filter(|comparison| comparison.input_id.starts_with("mal-"))
        .filter(|comparison| {
            matches!(
                comparison.result,
                ComparisonResult::Timeout
                    | ComparisonResult::Crash
                    | ComparisonResult::ProtocolError
            )
        })
        .count();
    anyhow::ensure!(
        legal_failures == 0,
        "{legal_failures} legal comparisons failed"
    );
    anyhow::ensure!(
        unbounded_malformed == 0,
        "{unbounded_malformed} malformed inputs crashed, timed out, or broke protocol"
    );
    Ok(())
}

fn run_probe(
    candidate: &Path,
    pdfium: &PdfiumSpec,
    fixture: &Path,
    output: &Path,
    timeout: Duration,
) -> Result<()> {
    let mut command = Command::new(candidate);
    command
        .arg("probe-is-generated")
        .arg("--pdf")
        .arg(fixture)
        .arg("--pdfium-library")
        .arg(&pdfium.library)
        .arg("--pdfium-release")
        .arg(&pdfium.release);
    let capture = capture_command(command, timeout, None)?;
    anyhow::ensure!(!capture.timed_out, "IsGenerated probe timed out");
    anyhow::ensure!(
        capture.status.as_ref().is_some_and(ExitStatus::success),
        "IsGenerated probe failed: {}",
        capture.stderr
    );
    let value: serde_json::Value = serde_json::from_str(capture.stdout.trim())?;
    atomic_write_json(
        &output.join(format!("is-generated-{}.json", pdfium.release)),
        &value,
    )
}

fn run_wrapper(
    runner: &Path,
    input: &BatchInput,
    pdfium: &PdfiumSpec,
    timeout: Duration,
) -> Result<Captured> {
    let mut command = Command::new(runner);
    command
        .arg("run")
        .arg("--pdf")
        .arg(&input.path)
        .arg("--input-id")
        .arg(&input.input_id)
        .arg("--pdfium-library")
        .arg(&pdfium.library)
        .arg("--pdfium-release")
        .arg(&pdfium.release);
    capture_command(command, timeout, None)
}

fn compare_captures(
    release: &str,
    input_id: &str,
    reference: &Captured,
    candidate: &Captured,
) -> ComparisonReport {
    let mut report = ComparisonReport {
        schema_version: SCHEMA_VERSION,
        pdfium_release: release.to_owned(),
        input_id: input_id.to_owned(),
        reference_exit: reference.status.as_ref().and_then(ExitStatus::code),
        candidate_exit: candidate.status.as_ref().and_then(ExitStatus::code),
        result: ComparisonResult::Equal,
        differences: Vec::new(),
    };
    if reference.timed_out || candidate.timed_out {
        report.result = ComparisonResult::Timeout;
        return report;
    }
    if reference
        .status
        .as_ref()
        .is_some_and(|status| status.code().is_none())
        || candidate
            .status
            .as_ref()
            .is_some_and(|status| status.code().is_none())
    {
        report.result = ComparisonResult::Crash;
        return report;
    }
    let reference: RunReport = match serde_json::from_str(reference.stdout.trim()) {
        Ok(value) => value,
        Err(error) => {
            report.result = ComparisonResult::ProtocolError;
            report.differences.push(Difference {
                path: "reference.stdout".to_owned(),
                reference: error.to_string(),
                candidate: String::new(),
                delta: None,
            });
            return report;
        }
    };
    let candidate: RunReport = match serde_json::from_str(candidate.stdout.trim()) {
        Ok(value) => value,
        Err(error) => {
            report.result = ComparisonResult::ProtocolError;
            report.differences.push(Difference {
                path: "candidate.stdout".to_owned(),
                reference: String::new(),
                candidate: error.to_string(),
                delta: None,
            });
            return report;
        }
    };
    if reference.schema_version != SCHEMA_VERSION || candidate.schema_version != SCHEMA_VERSION {
        report.result = ComparisonResult::ProtocolError;
        return report;
    }
    match (&reference.outcome, &candidate.outcome) {
        (RunOutcome::Success, RunOutcome::Success) => {
            compare_success(&mut report, &reference, &candidate)
        }
        (
            RunOutcome::Error {
                class: reference, ..
            },
            RunOutcome::Error {
                class: candidate, ..
            },
        ) if reference == candidate => report.result = ComparisonResult::EquivalentFailure,
        (reference, candidate) => {
            report.result = ComparisonResult::Different;
            report.differences.push(Difference {
                path: "outcome".to_owned(),
                reference: format!("{reference:?}"),
                candidate: format!("{candidate:?}"),
                delta: None,
            });
        }
    }
    report
}

fn compare_success(report: &mut ComparisonReport, reference: &RunReport, candidate: &RunReport) {
    exact(
        report,
        "pdfium_release",
        &reference.pdfium_release,
        &candidate.pdfium_release,
    );
    exact(
        report,
        "pdfium_library_sha256",
        &reference.pdfium_library_sha256,
        &candidate.pdfium_library_sha256,
    );
    exact(
        report,
        "pdf_sha256",
        &reference.pdf_sha256,
        &candidate.pdf_sha256,
    );
    if reference.pages.len() != candidate.pages.len() {
        difference(
            report,
            "pages.len",
            reference.pages.len(),
            candidate.pages.len(),
            None,
        );
        report.result = ComparisonResult::Different;
        return;
    }
    for (page_index, (reference, candidate)) in
        reference.pages.iter().zip(&candidate.pages).enumerate()
    {
        let prefix = format!("pages[{page_index}]");
        exact(
            report,
            &format!("{prefix}.page_number"),
            &reference.page_number,
            &candidate.page_number,
        );
        numeric(
            report,
            &format!("{prefix}.width_pt"),
            reference.width_pt,
            candidate.width_pt,
        );
        numeric(
            report,
            &format!("{prefix}.height_pt"),
            reference.height_pt,
            candidate.height_pt,
        );
        exact(
            report,
            &format!("{prefix}.rotate_degrees"),
            &reference.rotate_degrees,
            &candidate.rotate_degrees,
        );
        exact(
            report,
            &format!("{prefix}.text"),
            &reference.text,
            &candidate.text,
        );
        exact(
            report,
            &format!("{prefix}.render.width_px"),
            &reference.render.width_px,
            &candidate.render.width_px,
        );
        exact(
            report,
            &format!("{prefix}.render.height_px"),
            &reference.render.height_px,
            &candidate.render.height_px,
        );
        exact(
            report,
            &format!("{prefix}.render.rgba8_sha256"),
            &reference.render.rgba8_sha256,
            &candidate.render.rgba8_sha256,
        );
        if reference.characters.len() != candidate.characters.len() {
            difference(
                report,
                &format!("{prefix}.characters.len"),
                reference.characters.len(),
                candidate.characters.len(),
                None,
            );
            continue;
        }
        for (char_index, (reference, candidate)) in reference
            .characters
            .iter()
            .zip(&candidate.characters)
            .enumerate()
        {
            let prefix = format!("{prefix}.characters[{char_index}]");
            exact(
                report,
                &format!("{prefix}.index"),
                &reference.index,
                &candidate.index,
            );
            exact(
                report,
                &format!("{prefix}.unicode"),
                &reference.unicode,
                &candidate.unicode,
            );
            exact(
                report,
                &format!("{prefix}.code"),
                &reference.code,
                &candidate.code,
            );
            numeric(
                report,
                &format!("{prefix}.origin.x"),
                reference.origin.x,
                candidate.origin.x,
            );
            numeric(
                report,
                &format!("{prefix}.origin.y"),
                reference.origin.y,
                candidate.origin.y,
            );
            numeric(
                report,
                &format!("{prefix}.tight_box.left"),
                reference.tight_box.left,
                candidate.tight_box.left,
            );
            numeric(
                report,
                &format!("{prefix}.tight_box.bottom"),
                reference.tight_box.bottom,
                candidate.tight_box.bottom,
            );
            numeric(
                report,
                &format!("{prefix}.tight_box.right"),
                reference.tight_box.right,
                candidate.tight_box.right,
            );
            numeric(
                report,
                &format!("{prefix}.tight_box.top"),
                reference.tight_box.top,
                candidate.tight_box.top,
            );
            numeric(
                report,
                &format!("{prefix}.loose_box.left"),
                reference.loose_box.left,
                candidate.loose_box.left,
            );
            numeric(
                report,
                &format!("{prefix}.loose_box.bottom"),
                reference.loose_box.bottom,
                candidate.loose_box.bottom,
            );
            numeric(
                report,
                &format!("{prefix}.loose_box.right"),
                reference.loose_box.right,
                candidate.loose_box.right,
            );
            numeric(
                report,
                &format!("{prefix}.loose_box.top"),
                reference.loose_box.top,
                candidate.loose_box.top,
            );
        }
    }
    report.result = if report.differences.is_empty() {
        ComparisonResult::Equal
    } else {
        ComparisonResult::Different
    };
}

fn numeric(report: &mut ComparisonReport, path: &str, reference: f64, candidate: f64) {
    let delta = (reference - candidate).abs();
    if !delta.is_finite() || delta > GEOMETRY_TOLERANCE_PT {
        difference(report, path, reference, candidate, Some(delta));
    }
}

fn exact<T: std::fmt::Debug + PartialEq>(
    report: &mut ComparisonReport,
    path: &str,
    reference: &T,
    candidate: &T,
) {
    if reference != candidate {
        difference(report, path, reference, candidate, None);
    }
}

fn difference(
    report: &mut ComparisonReport,
    path: &str,
    reference: impl std::fmt::Debug,
    candidate: impl std::fmt::Debug,
    delta: Option<f64>,
) {
    if report.differences.len() < 100 {
        report.differences.push(Difference {
            path: path.to_owned(),
            reference: format!("{reference:?}"),
            candidate: format!("{candidate:?}"),
            delta,
        });
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BenchmarkSummary {
    schema_version: u32,
    runs: Vec<BenchmarkRun>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BenchmarkRun {
    backend: String,
    pdfium_release: String,
    mode: String,
    concurrency: usize,
    elapsed_us: u64,
    jobs: usize,
    pages: usize,
    throughput_pages_per_second: f64,
    p50_document_us: u64,
    p95_document_us: u64,
    peak_rss_kib: u64,
    rss_samples_kib: Vec<u64>,
    success: bool,
}

fn run_benchmark(args: BenchmarkArgs) -> Result<()> {
    validate_runners(&args.runners)?;
    let request = read_request(&args.request)?;
    fs::create_dir_all(&args.output)?;
    let timeout = Duration::from_secs(args.timeout_seconds);
    let thread_counts = parse_counts(&args.thread_counts)?;
    let process_counts = parse_counts(&args.process_counts)?;
    let mut runs = Vec::new();

    for pdfium in &args.runners.pdfiums {
        runs.push(run_batch_benchmark(
            "reference",
            &args.runners.reference_runner,
            pdfium,
            &args.request,
            &args
                .output
                .join(format!("reference-{}-t1.json", pdfium.release)),
            1,
            timeout,
        )?);
        for threads in &thread_counts {
            runs.push(run_batch_benchmark(
                "candidate",
                &args.runners.candidate_runner,
                pdfium,
                &args.request,
                &args
                    .output
                    .join(format!("candidate-{}-t{threads}.json", pdfium.release)),
                *threads,
                timeout,
            )?);
        }
        for processes in &process_counts {
            runs.push(run_process_benchmark(
                &args.runners.candidate_runner,
                pdfium,
                &request,
                &args.output,
                *processes,
                timeout,
            )?);
        }
    }
    atomic_write_json(
        &args.output.join("benchmark-summary.json"),
        &BenchmarkSummary {
            schema_version: SCHEMA_VERSION,
            runs,
        },
    )
}

fn run_batch_benchmark(
    backend: &str,
    runner: &Path,
    pdfium: &PdfiumSpec,
    request: &Path,
    checkpoint: &Path,
    threads: usize,
    timeout: Duration,
) -> Result<BenchmarkRun> {
    let _ = fs::remove_file(checkpoint);
    let mut command = Command::new(runner);
    command
        .arg("batch")
        .arg("--request")
        .arg(request)
        .arg("--pdfium-library")
        .arg(&pdfium.library)
        .arg("--pdfium-release")
        .arg(&pdfium.release)
        .arg("--checkpoint")
        .arg(checkpoint)
        .arg("--warmup-rounds")
        .arg("1");
    if backend == "candidate" {
        command.arg("--threads").arg(threads.to_string());
    }
    let capture = capture_command(
        command,
        timeout,
        Some(RssSampling {
            interval: Duration::from_millis(50),
            start_after: None,
        }),
    )?;
    benchmark_from_capture(backend, &pdfium.release, "threads", threads, capture)
}

fn run_process_benchmark(
    runner: &Path,
    pdfium: &PdfiumSpec,
    request: &BatchRequest,
    output: &Path,
    processes: usize,
    timeout: Duration,
) -> Result<BenchmarkRun> {
    let started = Instant::now();
    let captures = thread::scope(|scope| -> Result<Vec<Captured>> {
        let mut handles = Vec::new();
        for process_index in 0..processes {
            let partition = BatchRequest {
                schema_version: SCHEMA_VERSION,
                inputs: request
                    .inputs
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| index % processes == process_index)
                    .map(|(_, input)| input.clone())
                    .collect(),
            };
            let request_path = output.join(format!(
                "process-request-{}-p{processes}-{process_index}.json",
                pdfium.release
            ));
            let checkpoint = output.join(format!(
                "process-checkpoint-{}-p{processes}-{process_index}.json",
                pdfium.release
            ));
            atomic_write_json(&request_path, &partition)?;
            let _ = fs::remove_file(&checkpoint);
            handles.push(scope.spawn(move || {
                let mut command = Command::new(runner);
                command
                    .arg("batch")
                    .arg("--request")
                    .arg(request_path)
                    .arg("--pdfium-library")
                    .arg(&pdfium.library)
                    .arg("--pdfium-release")
                    .arg(&pdfium.release)
                    .arg("--checkpoint")
                    .arg(checkpoint)
                    .arg("--threads")
                    .arg("1")
                    .arg("--warmup-rounds")
                    .arg("1");
                capture_command(
                    command,
                    timeout,
                    Some(RssSampling {
                        interval: Duration::from_millis(50),
                        start_after: None,
                    }),
                )
            }));
        }
        handles
            .into_iter()
            .map(|handle| handle.join().expect("process benchmark thread panicked"))
            .collect()
    })?;
    let elapsed_us = started.elapsed().as_micros() as u64;
    let mut reports = Vec::new();
    let mut peak_rss_kib = 0;
    let mut rss_samples = Vec::new();
    let mut success = true;
    for capture in captures {
        success &= !capture.timed_out && capture.status.as_ref().is_some_and(ExitStatus::success);
        peak_rss_kib = peak_rss_kib.max(capture.peak_rss_kib);
        rss_samples.extend(capture.rss_samples_kib);
        if let Ok(report) = serde_json::from_str::<BatchReport>(capture.stdout.trim()) {
            reports.push(report);
        } else {
            success = false;
        }
    }
    let jobs = reports.iter().map(|report| report.jobs.len()).sum();
    let pages = reports
        .iter()
        .flat_map(|report| &report.jobs)
        .map(|job| job.page_count)
        .sum();
    let durations = reports
        .iter()
        .flat_map(|report| report.jobs.iter().map(|job| job.elapsed_us))
        .collect::<Vec<_>>();
    Ok(BenchmarkRun {
        backend: "candidate".to_owned(),
        pdfium_release: pdfium.release.clone(),
        mode: "processes".to_owned(),
        concurrency: processes,
        elapsed_us,
        jobs,
        pages,
        throughput_pages_per_second: throughput(pages, elapsed_us),
        p50_document_us: percentile(&durations, 0.50),
        p95_document_us: percentile(&durations, 0.95),
        peak_rss_kib,
        rss_samples_kib: rss_samples,
        success,
    })
}

fn benchmark_from_capture(
    backend: &str,
    release: &str,
    mode: &str,
    concurrency: usize,
    capture: Captured,
) -> Result<BenchmarkRun> {
    let report: BatchReport = serde_json::from_str(capture.stdout.trim())
        .with_context(|| format!("parse {backend} benchmark: {}", capture.stderr))?;
    let durations = report
        .jobs
        .iter()
        .map(|job| job.elapsed_us)
        .collect::<Vec<_>>();
    let pages = report.jobs.iter().map(|job| job.page_count).sum();
    Ok(BenchmarkRun {
        backend: backend.to_owned(),
        pdfium_release: release.to_owned(),
        mode: mode.to_owned(),
        concurrency,
        elapsed_us: capture.elapsed_us,
        jobs: report.jobs.len(),
        pages,
        throughput_pages_per_second: throughput(pages, capture.elapsed_us),
        p50_document_us: percentile(&durations, 0.50),
        p95_document_us: percentile(&durations, 0.95),
        peak_rss_kib: capture.peak_rss_kib,
        rss_samples_kib: capture.rss_samples_kib,
        success: !capture.timed_out
            && capture.status.as_ref().is_some_and(ExitStatus::success)
            && report.jobs.iter().all(|job| job.success),
    })
}

fn throughput(pages: usize, elapsed_us: u64) -> f64 {
    if elapsed_us == 0 {
        0.0
    } else {
        pages as f64 * 1_000_000.0 / elapsed_us as f64
    }
}

fn percentile(values: &[u64], percentile: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut values = values.to_vec();
    values.sort_unstable();
    let index = ((values.len() - 1) as f64 * percentile).ceil() as usize;
    values[index]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LongRunSummary {
    schema_version: u32,
    started_unix_seconds: u64,
    requested_rounds: u32,
    max_hours: f64,
    completed_rounds: u32,
    fixtures_completed: u64,
    failures: u64,
    rss_requires_investigation: bool,
    records: Vec<LongRunRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LongRunRecord {
    backend: String,
    pdfium_release: String,
    requested_rounds: u32,
    completed_rounds: u32,
    partial_round_fixtures: usize,
    elapsed_us: u64,
    jobs: usize,
    pages: usize,
    throughput_pages_per_second: f64,
    p50_document_us: u64,
    p95_document_us: u64,
    failed_jobs: usize,
    content_stability_failures: Vec<ContentStabilityFailure>,
    process_exit: Option<i32>,
    reached_time_limit: bool,
    post_warmup_rss_samples_kib: Vec<u64>,
    peak_rss_kib: u64,
    first_half_mean_rss_kib: f64,
    second_half_mean_rss_kib: f64,
    rss_growth_ratio: f64,
    rss_nondecreasing_fraction: f64,
    rss_monotonic_growth_detected: bool,
    rss_requires_investigation: bool,
    success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ContentStabilityFailure {
    fixture_id: String,
    round: u32,
    expected_sha256: String,
    actual_sha256: String,
}

fn run_long_run(args: LongRunArgs) -> Result<()> {
    validate_runners(&args.runners)?;
    let request = read_request(&args.request)?;
    anyhow::ensure!(
        request
            .inputs
            .iter()
            .all(|input| !input.input_id.starts_with("mal-")),
        "long-run request must contain only legal fixtures"
    );
    anyhow::ensure!(args.max_rounds > 0, "max-rounds must be at least one");
    anyhow::ensure!(
        args.max_hours.is_finite() && args.max_hours > 0.0,
        "max-hours must be positive"
    );
    fs::create_dir_all(&args.output)?;
    let summary_path = args.output.join("long-run-summary.json");
    let started = Instant::now();
    let deadline = Duration::from_secs_f64(args.max_hours * 3600.0);
    let started_unix_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let records = thread::scope(|scope| -> Result<Vec<LongRunRecord>> {
        let mut handles = Vec::new();
        let request_path = &args.request;
        let output = &args.output;
        let request = &request;
        for pdfium in &args.runners.pdfiums {
            for (backend, runner) in [
                ("reference", &args.runners.reference_runner),
                ("candidate", &args.runners.candidate_runner),
            ] {
                handles.push(scope.spawn(move || {
                    run_long_run_process(
                        backend,
                        runner,
                        pdfium,
                        request_path,
                        output,
                        request,
                        args.max_rounds,
                        started,
                        deadline,
                    )
                }));
            }
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("long-run monitor panicked"))?
            })
            .collect()
    })?;

    let completed_rounds = records
        .iter()
        .map(|record| record.completed_rounds)
        .min()
        .unwrap_or(0);
    let fixtures_completed = records.iter().map(|record| record.jobs as u64).sum::<u64>();
    let failures = records
        .iter()
        .map(|record| {
            record.failed_jobs as u64
                + record.content_stability_failures.len() as u64
                + u64::from(!record.success)
        })
        .sum();
    let rss_requires_investigation = records
        .iter()
        .any(|record| record.rss_requires_investigation);
    let summary = LongRunSummary {
        schema_version: SCHEMA_VERSION,
        started_unix_seconds,
        requested_rounds: args.max_rounds,
        max_hours: args.max_hours,
        completed_rounds,
        fixtures_completed,
        failures,
        rss_requires_investigation,
        records,
    };
    atomic_write_json(&summary_path, &summary)?;
    anyhow::ensure!(summary.failures == 0, "long-run failures detected");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_long_run_process(
    backend: &str,
    runner: &Path,
    pdfium: &PdfiumSpec,
    request_path: &Path,
    output: &Path,
    request: &BatchRequest,
    max_rounds: u32,
    started: Instant,
    deadline: Duration,
) -> Result<LongRunRecord> {
    let checkpoint = output.join(format!("checkpoint-{backend}-{}.json", pdfium.release));
    let warmup_marker = output.join(format!("warmup-complete-{backend}-{}.json", pdfium.release));
    if warmup_marker.exists() {
        fs::remove_file(&warmup_marker)?;
    }
    let remaining = deadline.saturating_sub(started.elapsed());
    anyhow::ensure!(
        !remaining.is_zero(),
        "long-run duration reached before spawn"
    );
    let mut command = Command::new(runner);
    command
        .arg("batch")
        .arg("--request")
        .arg(request_path)
        .arg("--pdfium-library")
        .arg(&pdfium.library)
        .arg("--pdfium-release")
        .arg(&pdfium.release)
        .arg("--checkpoint")
        .arg(&checkpoint)
        .arg("--iterations")
        .arg(max_rounds.to_string())
        .arg("--warmup-rounds")
        .arg("1")
        .arg("--warmup-complete-marker")
        .arg(&warmup_marker);
    if backend == "candidate" {
        command.arg("--threads").arg("1");
    }
    let capture = capture_command(
        command,
        remaining,
        Some(RssSampling {
            interval: Duration::from_secs(1),
            start_after: Some(warmup_marker),
        }),
    )?;
    let report: BatchReport = if checkpoint.is_file() {
        serde_json::from_slice(&fs::read(&checkpoint)?)?
    } else {
        serde_json::from_str(capture.stdout.trim())
            .with_context(|| format!("parse long-run {backend} output: {}", capture.stderr))?
    };
    analyze_long_run_report(backend, pdfium, request, max_rounds, capture, report)
}

fn analyze_long_run_report(
    backend: &str,
    pdfium: &PdfiumSpec,
    request: &BatchRequest,
    max_rounds: u32,
    capture: Captured,
    report: BatchReport,
) -> Result<LongRunRecord> {
    anyhow::ensure!(
        report.schema_version == SCHEMA_VERSION,
        "batch schema mismatch"
    );
    anyhow::ensure!(report.backend == backend, "batch backend mismatch");
    anyhow::ensure!(
        report.pdfium_release == pdfium.release,
        "batch PDFium release mismatch"
    );
    let fixture_ids = request
        .inputs
        .iter()
        .map(|input| input.input_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut round_fixtures: BTreeMap<u32, BTreeSet<&str>> = BTreeMap::new();
    let mut hashes = BTreeMap::<&str, &str>::new();
    let mut stability_failures = Vec::new();
    for job in &report.jobs {
        let (round, fixture_id) = parse_long_run_job_id(&job.input_id, max_rounds, &fixture_ids)?;
        anyhow::ensure!(round <= max_rounds, "job round exceeds requested rounds");
        anyhow::ensure!(
            round_fixtures.entry(round).or_default().insert(fixture_id),
            "duplicate fixture in round {round}: {fixture_id}"
        );
        if let Some(actual) = job.report_sha256.as_deref() {
            if let Some(expected) = hashes.get(fixture_id) {
                if *expected != actual {
                    stability_failures.push(ContentStabilityFailure {
                        fixture_id: fixture_id.to_owned(),
                        round,
                        expected_sha256: (*expected).to_owned(),
                        actual_sha256: actual.to_owned(),
                    });
                }
            } else {
                hashes.insert(fixture_id, actual);
            }
        }
    }
    let completed_rounds = (1..=max_rounds)
        .take_while(|round| {
            round_fixtures
                .get(round)
                .is_some_and(|fixtures| fixtures.len() == fixture_ids.len())
        })
        .count() as u32;
    let partial_round_fixtures = round_fixtures
        .get(&(completed_rounds + 1))
        .map_or(0, BTreeSet::len);
    let failed_jobs = report.jobs.iter().filter(|job| !job.success).count();
    let pages = report.jobs.iter().map(|job| job.page_count).sum();
    let durations = report
        .jobs
        .iter()
        .map(|job| job.elapsed_us)
        .collect::<Vec<_>>();
    let rss = rss_trend(&capture.rss_samples_kib);
    let reached_time_limit = capture.timed_out;
    let process_succeeded = capture.status.as_ref().is_some_and(ExitStatus::success);
    let finished_requested_rounds = completed_rounds == max_rounds;
    let success = failed_jobs == 0
        && stability_failures.is_empty()
        && (process_succeeded || (reached_time_limit && !finished_requested_rounds));
    Ok(LongRunRecord {
        backend: backend.to_owned(),
        pdfium_release: pdfium.release.clone(),
        requested_rounds: max_rounds,
        completed_rounds,
        partial_round_fixtures,
        elapsed_us: capture.elapsed_us,
        jobs: report.jobs.len(),
        pages,
        throughput_pages_per_second: throughput(pages, capture.elapsed_us),
        p50_document_us: percentile(&durations, 0.50),
        p95_document_us: percentile(&durations, 0.95),
        failed_jobs,
        content_stability_failures: stability_failures,
        process_exit: capture.status.as_ref().and_then(ExitStatus::code),
        reached_time_limit,
        post_warmup_rss_samples_kib: capture.rss_samples_kib,
        peak_rss_kib: rss.peak_kib,
        first_half_mean_rss_kib: rss.first_half_mean_kib,
        second_half_mean_rss_kib: rss.second_half_mean_kib,
        rss_growth_ratio: rss.growth_ratio,
        rss_nondecreasing_fraction: rss.nondecreasing_fraction,
        rss_monotonic_growth_detected: rss.monotonic_growth_detected,
        rss_requires_investigation: rss.requires_investigation,
        success,
    })
}

fn parse_long_run_job_id<'a>(
    job_id: &'a str,
    max_rounds: u32,
    fixture_ids: &BTreeSet<&str>,
) -> Result<(u32, &'a str)> {
    if max_rounds == 1 && fixture_ids.contains(job_id) {
        return Ok((1, job_id));
    }
    let (round, fixture_id) = job_id
        .strip_prefix("round-")
        .and_then(|value| value.split_once(':'))
        .context("invalid long-run job id")?;
    let round = round.parse::<u32>().context("invalid long-run round")?;
    anyhow::ensure!(
        fixture_ids.contains(fixture_id),
        "unknown fixture {fixture_id}"
    );
    Ok((round, fixture_id))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RssTrend {
    peak_kib: u64,
    first_half_mean_kib: f64,
    second_half_mean_kib: f64,
    growth_ratio: f64,
    nondecreasing_fraction: f64,
    monotonic_growth_detected: bool,
    requires_investigation: bool,
}

fn rss_trend(samples: &[u64]) -> RssTrend {
    let midpoint = samples.len().div_ceil(2);
    let first_mean = mean(&samples[..midpoint]);
    let second_mean = mean(&samples[midpoint..]);
    let growth_ratio = if first_mean > 0.0 {
        (second_mean - first_mean) / first_mean
    } else {
        0.0
    };
    let nondecreasing = samples.windows(2).filter(|pair| pair[1] >= pair[0]).count();
    let nondecreasing_fraction = if samples.len() > 1 {
        nondecreasing as f64 / (samples.len() - 1) as f64
    } else {
        0.0
    };
    let monotonic_growth_detected = samples.len() >= 10
        && nondecreasing_fraction >= 0.95
        && samples.last().copied().unwrap_or(0) as f64
            > samples.first().copied().unwrap_or(0) as f64 * 1.05;
    RssTrend {
        peak_kib: samples.iter().copied().max().unwrap_or(0),
        first_half_mean_kib: first_mean,
        second_half_mean_kib: second_mean,
        growth_ratio,
        nondecreasing_fraction,
        monotonic_growth_detected,
        requires_investigation: growth_ratio > 0.20 || monotonic_growth_detected,
    }
}

fn mean(values: &[u64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().map(|value| *value as f64).sum::<f64>() / values.len() as f64
    }
}

#[derive(Debug)]
struct Captured {
    status: Option<ExitStatus>,
    timed_out: bool,
    stdout: String,
    stderr: String,
    elapsed_us: u64,
    peak_rss_kib: u64,
    rss_samples_kib: Vec<u64>,
}

struct RssSampling {
    interval: Duration,
    start_after: Option<PathBuf>,
}

fn capture_command(
    mut command: Command,
    timeout: Duration,
    rss_sampling: Option<RssSampling>,
) -> Result<Captured> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().context("spawn runner")?;
    let pid = child.id();
    let stdout_pipe = child.stdout.take().context("runner stdout pipe")?;
    let stderr_pipe = child.stderr.take().context("runner stderr pipe")?;
    let (status, stdout, stderr, rss_samples) = thread::scope(|scope| -> Result<_> {
        let stdout_reader = scope.spawn(move || -> std::io::Result<String> {
            let mut pipe = stdout_pipe;
            let mut output = String::new();
            pipe.read_to_string(&mut output)?;
            Ok(output)
        });
        let stderr_reader = scope.spawn(move || -> std::io::Result<String> {
            let mut pipe = stderr_pipe;
            let mut output = String::new();
            pipe.read_to_string(&mut output)?;
            Ok(output)
        });
        let mut rss_samples = Vec::new();
        let mut next_rss_sample = Instant::now();
        let status = loop {
            if let Some(status) = child.wait_timeout(Duration::from_millis(50))? {
                break Some(status);
            }
            if let Some(sampling) = &rss_sampling {
                let warmup_complete = sampling
                    .start_after
                    .as_ref()
                    .map_or(true, |path| path.is_file());
                if warmup_complete && Instant::now() >= next_rss_sample {
                    if let Some(rss) = process_rss_kib(pid) {
                        rss_samples.push(rss);
                    }
                    next_rss_sample = Instant::now() + sampling.interval;
                }
            }
            if started.elapsed() >= timeout {
                child.kill().context("kill timed-out runner")?;
                let _ = child.wait();
                break None;
            }
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
        Ok((status, stdout, stderr, rss_samples))
    })?;
    let timed_out = status.is_none();
    Ok(Captured {
        status,
        timed_out,
        stdout,
        stderr,
        elapsed_us: started.elapsed().as_micros() as u64,
        peak_rss_kib: rss_samples.iter().copied().max().unwrap_or(0),
        rss_samples_kib: rss_samples,
    })
}

fn process_rss_kib(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn write_capture(path: &Path, capture: &Captured) -> Result<()> {
    let parsed =
        serde_json::from_str::<serde_json::Value>(capture.stdout.trim()).unwrap_or_else(|_| {
            serde_json::json!({
                "stdout": capture.stdout,
                "stderr": capture.stderr,
                "timed_out": capture.timed_out,
                "exit": capture.status.as_ref().and_then(ExitStatus::code),
            })
        });
    atomic_write_json(path, &parsed)
}

fn discover_corpus(repo_root: &Path) -> Result<BatchRequest> {
    let root = repo_root.join("corpus/fixtures");
    let mut inputs = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let input_id = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path().join(format!("{input_id}.pdf"));
        if path.is_file() {
            inputs.push(BatchInput {
                input_id,
                path: path.canonicalize()?.display().to_string(),
            });
        }
    }
    inputs.sort_by(|left, right| left.input_id.cmp(&right.input_id));
    Ok(BatchRequest {
        schema_version: SCHEMA_VERSION,
        inputs,
    })
}

fn read_request(path: &Path) -> Result<BatchRequest> {
    let request: BatchRequest = serde_json::from_slice(&fs::read(path)?)?;
    anyhow::ensure!(
        request.schema_version == SCHEMA_VERSION,
        "request schema mismatch"
    );
    Ok(request)
}

fn load_matrix_checkpoint(path: &Path) -> Result<Vec<ComparisonReport>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let summary: MatrixSummary = serde_json::from_slice(&fs::read(path)?)?;
    anyhow::ensure!(
        summary.schema_version == SCHEMA_VERSION,
        "matrix schema mismatch"
    );
    Ok(summary.comparisons)
}

fn validate_runners(args: &RunnerArgs) -> Result<()> {
    anyhow::ensure!(args.reference_runner.is_file(), "reference runner missing");
    anyhow::ensure!(args.candidate_runner.is_file(), "candidate runner missing");
    anyhow::ensure!(
        !args.pdfiums.is_empty(),
        "at least one --pdfium is required"
    );
    let mut releases = BTreeMap::new();
    for pdfium in &args.pdfiums {
        anyhow::ensure!(
            pdfium.library.is_file(),
            "missing {}",
            pdfium.library.display()
        );
        anyhow::ensure!(
            releases.insert(&pdfium.release, ()).is_none(),
            "duplicate release {}",
            pdfium.release
        );
    }
    Ok(())
}

fn parse_counts(value: &str) -> Result<Vec<usize>> {
    let counts = value
        .split(',')
        .map(|value| {
            value
                .trim()
                .parse::<usize>()
                .context("invalid concurrency count")
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        !counts.is_empty() && counts.iter().all(|count| *count > 0),
        "concurrency counts must be positive"
    );
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_upper_rank() {
        assert_eq!(percentile(&[1, 2, 3, 4], 0.50), 3);
        assert_eq!(percentile(&[1, 2, 3, 4], 0.95), 4);
    }

    #[test]
    fn parses_pdfium_spec() {
        let spec = parse_pdfium("chromium-8009=/tmp/libpdfium.dylib").unwrap();
        assert_eq!(spec.release, "chromium-8009");
        assert_eq!(spec.library, PathBuf::from("/tmp/libpdfium.dylib"));
    }

    #[test]
    fn rss_trend_flags_twenty_percent_growth() {
        let trend = rss_trend(&[100, 100, 100, 130, 130, 130]);
        assert_eq!(trend.first_half_mean_kib, 100.0);
        assert_eq!(trend.second_half_mean_kib, 130.0);
        assert!(trend.requires_investigation);
    }

    #[test]
    fn rss_trend_accepts_flat_samples() {
        let trend = rss_trend(&[100; 12]);
        assert_eq!(trend.growth_ratio, 0.0);
        assert!(!trend.monotonic_growth_detected);
        assert!(!trend.requires_investigation);
    }
}
