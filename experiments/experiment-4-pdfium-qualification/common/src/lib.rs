use std::fs;
use std::io::Write;
use std::path::Path;
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
}
