use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lopdf::Document as LopdfDocument;

use crate::engine::{LayoutDetector, LayoutRegion, PageCharSnapshot, PdfEngine, RgbaImage};
use crate::error::Result;
use crate::event::{Diagnostics, EventSink, PageDegradeReason, RecoveryKind, Stage};
use crate::geometry::PageFrame;
use crate::il;
use crate::scan::{PageClass, PageEvidence};
use crate::translate::{Glossary, PreparedTranslation, Sleeper, ThreadSleeper, Translator};
use crate::walk::{WalkedChar, WalkedContentStream};
use crate::write::{PageRewrite, WriteReport};

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub baseline_tolerance_pt: f64,
    pub target_language: String,
    pub output_fonts: Option<OutputFonts>,
    pub user_glossary: Glossary,
    pub auto_terms: bool,
    pub dump_glossary: Option<PathBuf>,
    pub cache_path: Option<PathBuf>,
    pub max_concurrency: usize,
    pub sleeper: Arc<dyn Sleeper>,
    pub strict: bool,
    pub translate_table: bool,
    pub strip_link_borders: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            baseline_tolerance_pt: 0.001,
            target_language: "zh-CN".to_owned(),
            output_fonts: None,
            user_glossary: Glossary::default(),
            auto_terms: true,
            dump_glossary: None,
            cache_path: None,
            max_concurrency: 4,
            sleeper: Arc::new(ThreadSleeper),
            strict: false,
            translate_table: false,
            strip_link_borders: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputFont {
    pub bytes: Vec<u8>,
    pub postscript_name: String,
    pub source: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct OutputFonts {
    pub regular: OutputFont,
    pub bold: OutputFont,
    pub fallback_regular: OutputFont,
    pub fallback_bold: OutputFont,
}

pub struct PassContext<'a> {
    pub engine: &'a dyn PdfEngine,
    pub layout_detector: &'a dyn LayoutDetector,
    pub translator: &'a dyn Translator,
    pub events: &'a dyn EventSink,
    pub snapshots: Option<&'a dyn PassSnapshotSink>,
    pub config: PipelineConfig,
}

#[derive(Debug, Default)]
pub(crate) struct CharacterAlignment {
    pub engine_indices_by_walk: Vec<Option<usize>>,
    pub weak_unicode_conflicts: BTreeSet<usize>,
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
    pub(crate) prepared_translations:
        std::collections::BTreeMap<(usize, usize), PreparedTranslation>,
    pub(crate) restored_translations:
        std::collections::BTreeMap<(usize, usize), crate::translate::RestoredTranslation>,
    pub(crate) placeholder_violations:
        std::collections::BTreeMap<(usize, usize), crate::translate::PlaceholderViolation>,
    pub(crate) suspicious_echoes: BTreeSet<(usize, usize)>,
    pub(crate) glossary: Glossary,
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
            prepared_translations: std::collections::BTreeMap::new(),
            restored_translations: std::collections::BTreeMap::new(),
            placeholder_violations: std::collections::BTreeMap::new(),
            suspicious_echoes: BTreeSet::new(),
            glossary: Glossary::default(),
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
            prepared_translations: std::collections::BTreeMap::new(),
            restored_translations: std::collections::BTreeMap::new(),
            placeholder_violations: std::collections::BTreeMap::new(),
            suspicious_echoes: BTreeSet::new(),
            glossary: Glossary::default(),
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
    pub page_id: lopdf::ObjectId,
    pub geometry: il::PageGeometry,
    pub frame: Option<PageFrame>,
    pub evidence: PageEvidence,
    pub class: Option<PageClass>,
    // ADR-0013 §2: 页级降级标记。置位后该页不再进入后续 pass，也不产生 rewrite。
    pub degraded: Option<PageDegradeReason>,
    pub recoveries: BTreeSet<RecoveryKind>,
    pub walked_characters: Vec<WalkedChar>,
    pub vector_paths: Vec<crate::walk::WalkedVectorPath>,
    pub inline_images: Vec<crate::walk::WalkedInlineImage>,
    pub content_streams: Vec<WalkedContentStream>,
    pub engine_characters: Vec<PageCharSnapshot>,
    pub character_alignment: CharacterAlignment,
    pub layout_regions: Vec<LayoutRegion>,
    pub input_raster: Option<RgbaImage>,
}

impl ExtractedPage {
    /// 只有未降级的内容页会被翻译改写；其余（空白页、扫描页、降级页）一律整页透传。
    pub(crate) fn is_translatable(&self) -> bool {
        self.class == Some(PageClass::Content) && self.degraded.is_none()
    }
}
