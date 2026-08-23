use std::path::{Path, PathBuf};

use lopdf::Document as LopdfDocument;

use crate::engine::{LayoutDetector, LayoutRegion, PageCharSnapshot, PdfEngine, RgbaImage};
use crate::error::Result;
use crate::event::{Diagnostics, EventSink, Stage};
use crate::il;
use crate::translate::Translator;
use crate::walk::WalkedChar;
use crate::write::{PageRewrite, WriteReport};

#[derive(Debug, Clone, Copy)]
pub struct PipelineConfig {
    pub baseline_tolerance_pt: f64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            baseline_tolerance_pt: 0.001,
        }
    }
}

pub struct PassContext<'a> {
    pub engine: &'a dyn PdfEngine,
    pub layout_detector: &'a dyn LayoutDetector,
    pub translator: &'a dyn Translator,
    pub events: &'a dyn EventSink,
    pub snapshots: Option<&'a dyn PassSnapshotSink>,
    pub config: PipelineConfig,
}

pub trait PassSnapshotSink: Send + Sync {
    fn write_snapshot(
        &self,
        pass_index: usize,
        stage: Stage,
        snapshot: &il::Document,
    ) -> Result<()>;
}

pub struct Document {
    input_path: PathBuf,
    output_path: Option<PathBuf>,
    pub original_bytes: Vec<u8>,
    pub pdf: Option<LopdfDocument>,
    pub il: il::Document,
    pub diagnostics: Diagnostics,
    pub(crate) extracted_pages: Vec<ExtractedPage>,
    pub(crate) rewrites: Vec<PageRewrite>,
    pub(crate) write_report: Option<WriteReport>,
}

impl Document {
    #[must_use]
    pub fn new(input_path: impl Into<PathBuf>, output_path: impl Into<PathBuf>) -> Self {
        Self::for_translation(input_path, output_path)
    }

    #[must_use]
    pub fn for_translation(
        input_path: impl Into<PathBuf>,
        output_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            input_path: input_path.into(),
            output_path: Some(output_path.into()),
            original_bytes: Vec::new(),
            pdf: None,
            il: il::Document::default(),
            diagnostics: Diagnostics::default(),
            extracted_pages: Vec::new(),
            rewrites: Vec::new(),
            write_report: None,
        }
    }

    #[must_use]
    pub fn for_inspection(input_path: impl Into<PathBuf>) -> Self {
        Self {
            input_path: input_path.into(),
            output_path: None,
            original_bytes: Vec::new(),
            pdf: None,
            il: il::Document::default(),
            diagnostics: Diagnostics::default(),
            extracted_pages: Vec::new(),
            rewrites: Vec::new(),
            write_report: None,
        }
    }

    #[must_use]
    pub fn input_path(&self) -> &Path {
        &self.input_path
    }

    #[must_use]
    pub fn output_path(&self) -> Option<&Path> {
        self.output_path.as_deref()
    }
}

pub(crate) struct ExtractedPage {
    pub index: usize,
    pub geometry: il::PageGeometry,
    pub walked_characters: Vec<WalkedChar>,
    pub engine_characters: Vec<PageCharSnapshot>,
    pub layout_regions: Vec<LayoutRegion>,
    pub input_raster: Option<RgbaImage>,
}
