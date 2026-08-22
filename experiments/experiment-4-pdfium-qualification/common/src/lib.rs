use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;
pub const BACKEND_REVISION_PDFIUM_RENDER: &str = "pdfium-render@0.9.1/pdfium_7763";
pub const BACKEND_REVISION_FIRECRAWL: &str =
    "firecrawl-pdfium@1a4c91d0c5f80c0da779088ba241bf1e45271cd5";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunReport {
    pub schema_version: u32,
    pub backend: String,
    pub backend_revision: String,
    pub pdfium_release: String,
    pub pdfium_library_sha256: String,
    pub input_id: String,
    pub pdf_sha256: String,
    pub pages: Vec<PageReport>,
    pub timings: StageTimings,
    pub outcome: RunOutcome,
    pub process_exit: i32,
}

impl RunReport {
    #[allow(clippy::too_many_arguments)]
    pub fn error(
        backend: &str,
        backend_revision: &str,
        pdfium_release: &str,
        pdfium_library_sha256: String,
        input_id: String,
        pdf_sha256: String,
        timings: StageTimings,
        class: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            backend: backend.to_owned(),
            backend_revision: backend_revision.to_owned(),
            pdfium_release: pdfium_release.to_owned(),
            pdfium_library_sha256,
            input_id,
            pdf_sha256,
            pages: Vec::new(),
            timings,
            outcome: RunOutcome::Error {
                class: class.to_owned(),
                message: message.into(),
            },
            process_exit: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Error { class: String, message: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageTimings {
    pub library_load_us: u64,
    pub document_load_us: u64,
    pub text_us: u64,
    pub render_us: u64,
    pub total_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PageReport {
    pub page_number: u32,
    pub width_pt: f64,
    pub height_pt: f64,
    pub rotate_degrees: i32,
    pub text: String,
    pub characters: Vec<CharacterReport>,
    pub render: RenderReport,
    pub timings: PageTimings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CharacterReport {
    pub index: u32,
    pub unicode: Option<String>,
    pub code: u32,
    pub origin: Point,
    pub tight_box: Rect,
    pub loose_box: Rect,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderReport {
    pub width_px: u32,
    pub height_px: u32,
    pub rgba8_sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PageTimings {
    pub text_us: u64,
    pub render_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchRequest {
    pub schema_version: u32,
    pub inputs: Vec<BatchInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchInput {
    pub input_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchReport {
    pub schema_version: u32,
    pub backend: String,
    pub backend_revision: String,
    pub pdfium_release: String,
    pub threads: usize,
    pub elapsed_us: u64,
    pub jobs: Vec<BatchJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchJob {
    pub input_id: String,
    pub report_sha256: Option<String>,
    pub elapsed_us: u64,
    pub page_count: usize,
    pub success: bool,
    pub error_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchJobCheckpoint {
    pub schema_version: u32,
    pub backend: String,
    pub backend_revision: String,
    pub pdfium_release: String,
    pub threads: usize,
    pub job: BatchJob,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComparisonReport {
    pub schema_version: u32,
    pub pdfium_release: String,
    pub input_id: String,
    pub reference_exit: Option<i32>,
    pub candidate_exit: Option<i32>,
    pub result: ComparisonResult,
    pub differences: Vec<Difference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonResult {
    Equal,
    EquivalentFailure,
    Different,
    Timeout,
    Crash,
    ProtocolError,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Difference {
    pub path: String,
    pub reference: String,
    pub candidate: String,
    pub delta: Option<f64>,
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

pub fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

pub fn text_from_character_codes(characters: &[CharacterReport]) -> String {
    let mut units = Vec::with_capacity(characters.len());
    for character in characters {
        if let Ok(unit) = u16::try_from(character.code) {
            units.push(unit);
        } else if let Some(value) = char::from_u32(character.code) {
            let mut encoded = [0; 2];
            units.extend_from_slice(value.encode_utf16(&mut encoded));
        } else {
            units.push(char::REPLACEMENT_CHARACTER as u16);
        }
    }
    String::from_utf16_lossy(&units)
}

pub fn report_sha256(report: &RunReport) -> Result<String> {
    Ok(sha256_bytes(&serde_json::to_vec(report)?))
}

pub fn content_sha256(report: &RunReport) -> Result<String> {
    let mut pages = report.pages.clone();
    for page in &mut pages {
        page.timings = PageTimings::default();
    }
    Ok(sha256_bytes(&serde_json::to_vec(&pages)?))
}

pub fn batch_job_checkpoint_dir(checkpoint_path: &Path) -> Result<PathBuf> {
    let parent = checkpoint_path
        .parent()
        .with_context(|| format!("{} has no parent", checkpoint_path.display()))?;
    let file_name = checkpoint_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("checkpoint file name is not UTF-8")?;
    Ok(parent.join(format!("{file_name}.jobs")))
}

pub fn atomic_write_batch_job_checkpoint(
    checkpoint_path: &Path,
    backend: &str,
    backend_revision: &str,
    pdfium_release: &str,
    threads: usize,
    job: &BatchJob,
) -> Result<()> {
    let directory = batch_job_checkpoint_dir(checkpoint_path)?;
    let file_name = format!("{}.json", sha256_bytes(job.input_id.as_bytes()));
    atomic_write_json(
        &directory.join(file_name),
        &BatchJobCheckpoint {
            schema_version: SCHEMA_VERSION,
            backend: backend.to_owned(),
            backend_revision: backend_revision.to_owned(),
            pdfium_release: pdfium_release.to_owned(),
            threads,
            job: job.clone(),
        },
    )
}

pub fn load_batch_job_checkpoints(
    checkpoint_path: &Path,
    backend: &str,
    backend_revision: &str,
    pdfium_release: &str,
    threads: usize,
) -> Result<BTreeMap<String, BatchJob>> {
    let directory = batch_job_checkpoint_dir(checkpoint_path)?;
    if !directory.is_dir() {
        return Ok(BTreeMap::new());
    }
    let mut jobs = BTreeMap::new();
    for entry in fs::read_dir(&directory)
        .with_context(|| format!("read checkpoint directory {}", directory.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let checkpoint: BatchJobCheckpoint = serde_json::from_slice(&fs::read(entry.path())?)?;
        anyhow::ensure!(
            checkpoint.schema_version == SCHEMA_VERSION,
            "job checkpoint schema mismatch"
        );
        anyhow::ensure!(
            checkpoint.backend == backend,
            "job checkpoint backend mismatch"
        );
        anyhow::ensure!(
            checkpoint.backend_revision == backend_revision,
            "job checkpoint backend revision mismatch"
        );
        anyhow::ensure!(
            checkpoint.pdfium_release == pdfium_release,
            "job checkpoint PDFium release mismatch"
        );
        anyhow::ensure!(
            checkpoint.threads == threads,
            "job checkpoint thread count mismatch"
        );
        let expected_file_name =
            format!("{}.json", sha256_bytes(checkpoint.job.input_id.as_bytes()));
        anyhow::ensure!(
            entry.file_name().to_string_lossy() == expected_file_name,
            "job checkpoint file name mismatch"
        );
        let input_id = checkpoint.job.input_id.clone();
        anyhow::ensure!(
            jobs.insert(input_id.clone(), checkpoint.job).is_none(),
            "duplicate job checkpoint {input_id}"
        );
    }
    Ok(jobs)
}

pub fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("checkpoint file name is not UTF-8")?;
    let temporary = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let mut file =
        fs::File::create(&temporary).with_context(|| format!("create {}", temporary.display()))?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path).with_context(|| format!("publish {}", path.display()))?;
    Ok(())
}

pub fn atomic_write_text(path: &Path, value: &str) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("checkpoint file name is not UTF-8")?;
    let temporary = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let mut file =
        fs::File::create(&temporary).with_context(|| format!("create {}", temporary.display()))?;
    file.write_all(value.as_bytes())?;
    if !value.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    fs::rename(&temporary, path).with_context(|| format!("publish {}", path.display()))?;
    Ok(())
}

pub fn error_class(error: &anyhow::Error) -> String {
    let message = format!("{error:#}").to_ascii_lowercase();
    if message.contains("password") || message.contains("security") {
        "encrypted".to_owned()
    } else if message.contains("too large") || message.contains("limit") {
        "resource_limit".to_owned()
    } else if message.contains("page") {
        "page_error".to_owned()
    } else if message.contains("pdf") || message.contains("format") {
        "invalid_pdf".to_owned()
    } else if message.contains("library") || message.contains("symbol") {
        "pdfium_library".to_owned()
    } else {
        "backend_error".to_owned()
    }
}

pub fn micros_since(start: std::time::Instant) -> u64 {
    duration_us(start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_lower_hex() {
        assert_eq!(
            sha256_bytes(b"mimus"),
            "36ffc445cba567b1654e7b7237ff6a0d270cd7d94ab2295c6bb02a1f48f9f184"
        );
    }

    #[test]
    fn derives_job_checkpoint_directory_from_file_name() {
        assert_eq!(
            batch_job_checkpoint_dir(Path::new("results/candidate.json")).unwrap(),
            PathBuf::from("results/candidate.json.jobs")
        );
    }

    #[test]
    fn builds_canonical_text_from_pdfium_character_codes() {
        let characters = [0x41, 0x02, 0x1f600, 0xd83d, 0xde00, 0x11_0000]
            .into_iter()
            .enumerate()
            .map(|(index, code)| CharacterReport {
                index: index as u32,
                unicode: char::from_u32(code).map(|value| value.to_string()),
                code,
                origin: Point { x: 0.0, y: 0.0 },
                tight_box: Rect {
                    left: 0.0,
                    bottom: 0.0,
                    right: 0.0,
                    top: 0.0,
                },
                loose_box: Rect {
                    left: 0.0,
                    bottom: 0.0,
                    right: 0.0,
                    top: 0.0,
                },
            })
            .collect::<Vec<_>>();

        assert_eq!(
            text_from_character_codes(&characters),
            "A\u{2}\u{1f600}\u{1f600}\u{fffd}"
        );
    }
}
