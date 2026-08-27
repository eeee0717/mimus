use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::Serialize;

use crate::error::{ExitCategory, InternalReason, MimusError, Result, RetryReason};
use crate::il;

// CONTEXT "双 schema_version": CLI 事件协议与 IL 的演进节奏不同，禁止共用版本号。
pub const SCHEMA_VERSION: u32 = 2;
pub const MAX_DIAGNOSTICS: usize = 500;
pub const MAX_DIAGNOSTICS_PER_ID: usize = 25;
pub const MINIMAL_SERIALIZATION_ERROR_LINE: &[u8] = b"{\"schema_version\":2,\"event\":\"error\",\"category\":\"internal\",\"reason\":\"event_serialization\",\"message\":\"could not serialize protocol event\",\"hint\":null}\n";

fn usize_is_zero(value: &usize) -> bool {
    *value == 0
}

fn f64_is_zero(value: &f64) -> bool {
    *value == 0.0
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Event {
    pub schema_version: u32,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Event {
    #[must_use]
    pub const fn new(kind: EventKind) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    ConfigurationResolved {
        #[serde(flatten)]
        configuration: Box<ConfigurationResolved>,
    },
    TranslationCache {
        page_index: usize,
        paragraph_index: usize,
        status: CacheStatus,
    },
    StageStarted {
        stage: Stage,
    },
    StageFinished {
        stage: Stage,
    },
    PageProgress {
        stage: Stage,
        page_index: usize,
        total_pages: usize,
    },
    Diagnostic {
        #[serde(flatten)]
        diagnostic: DiagnosticEvent,
    },
    Result {
        #[serde(flatten)]
        result: ResultPayload,
        pages: usize,
        warnings: usize,
    },
    Error {
        category: ExitCategory,
        reason: crate::error::ErrorReason,
        message: String,
        hint: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        scanned_pages: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_pages: Option<usize>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ConfigurationResolved {
    pub backend: String,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub target_language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_regular_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_regular_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_bold_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_bold_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_fallback_regular_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_fallback_regular_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_fallback_bold_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_fallback_bold_sha256: Option<String>,
    pub layout_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_model_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_model_sha256: Option<String>,
    pub auto_terms: bool,
    pub glossary_fingerprint: String,
    pub cache_enabled: bool,
    pub cache_path: Option<String>,
    pub concurrency: usize,
    pub strict: bool,
    pub translate_table: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    Hit,
    Miss,
}

impl EventKind {
    #[must_use]
    pub fn from_error(error: &MimusError) -> Self {
        let (scanned_pages, total_pages) = error
            .scan_counts()
            .map_or((None, None), |(scanned, total)| {
                (Some(scanned), Some(total))
            });
        Self::Error {
            category: error.category(),
            reason: error.reason(),
            message: error.to_string(),
            hint: error.hint().map(str::to_owned),
            scanned_pages,
            total_pages,
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Result { .. } | Self::Error { .. })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum ResultPayload {
    Translate {
        output: String,
        translate_table: bool,
    },
    Inspect {
        il: il::Document,
    },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Parse,
    ScanDetect,
    Layout,
    ParagraphFind,
    StylesAndFormulas,
    ExtractTerms,
    Translate,
    Typeset,
    FontEmbed,
    Write,
}

impl Stage {
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::ScanDetect => "scan_detect",
            Self::Layout => "layout",
            Self::ParagraphFind => "paragraph_find",
            Self::StylesAndFormulas => "styles_and_formulas",
            Self::ExtractTerms => "extract_terms",
            Self::Translate => "translate",
            Self::Typeset => "typeset",
            Self::FontEmbed => "font_embed",
            Self::Write => "write",
        }
    }
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn emit(&self, _event: Event) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct RecordingEventSink {
    events: Mutex<Vec<Event>>,
}

impl RecordingEventSink {
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().expect("event mutex poisoned").clone()
    }
}

impl EventSink for RecordingEventSink {
    fn emit(&self, event: Event) -> Result<()> {
        self.events
            .lock()
            .expect("event mutex poisoned")
            .push(event);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticId {
    EngineBaselineMismatch,
    EngineCharacterMismatch,
    EngineCharacterAlignment,
    ScanSummary,
    PageDegraded,
    ContentRecovered,
    TranslationRetry,
    PlaceholderRetry,
    DegradationSummary,
    TranslationIdentity,
    SuspiciousEcho,
    SuspiciousTranslationEchoRate,
    PlaceholderViolation,
    TranslationFailureProfile,
    MathPassthrough,
    UnsupportedOutputGlyph,
    SingleLineBoundsExpanded,
    MultiLineBoundsExpanded,
    DroppedDiagnostics,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct DroppedDiagnosticCount {
    pub id: DiagnosticId,
    pub count: usize,
}

/// 有界宽容 walk 从畸形 content stream 里恢复出来的一处偏离（ADR-0013 §3）。
/// 与降级相反：这一页照常翻译，但它的写回结果不再与输入逐字节同源，
/// 所以恢复本身必须报告出来。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryKind {
    /// 操作符前有多余操作数；仅消费尾部 arity，其余在该边界丢弃。
    ArityExcess,
    /// 操作符缺少所需操作数，原子跳过且不改变状态。
    ArityShort,
    /// 操作数数量足够但类型不符，原子跳过且不改变状态。
    InvalidOperands,
    /// 数字与状态操作符粘连（如 `12Tf`），按最长白名单后缀拆开。
    GluedToken,
    /// 双小数点数字（如 `10.5.3`）在第二个小数点处分成两个操作数。
    DoubleDecimal,
    /// 未知操作符出现在 `BX`/`EX` 之外，被跳过。
    UnknownOperator,
    /// `EX` 没有对应的 `BX`，兼容深度保持在零。
    CompatibilityUnderflow,
    /// 页尾仍有未闭合的 `BX`，局部状态被丢弃。
    CompatibilityUnclosed,
    /// `Q` 没有对应的 `q`，保持 base graphics state。
    GraphicsStateUnderflow,
    /// 页尾仍有未闭合的 `q`，不影响此前已产出的字符。
    GraphicsStateUnclosed,
    /// 文本操作符出现在任何 `BT` 之前，按隐式 `BT`（`Tm` 为单位阵）处理（STREAM-05）。
    ImplicitTextObject,
    /// 文本对象内部再次出现 `BT`，按规范重置文本矩阵。
    NestedTextObject,
    /// `ET` 出现在文本对象之外，被原子跳过。
    UnexpectedTextEnd,
    /// 页尾仍处于显式文本对象内，隐式闭合。
    TextObjectUnclosed,
    /// `TJ` 数组里既非字符串也非数字的元素被跳过，字距按 0 计（STREAM-11）。
    SkippedTjElement,
    /// 过滤后的 inline image 没有编码长度，只能有界扫描 `EI`。
    InlineImageEiScan,
    /// 页尾有未被任何操作符消费的操作数，被丢弃。
    DanglingOperands,
    /// Form 直接再次调用自身；按对象 ID active path 截断。
    SelfRecursiveForm,
    /// Form 调用路径回到更早的祖先对象；按对象 ID active path 截断。
    MutuallyRecursiveForm,
    /// Form 调用链超过 64 层，当前 `Do` 被原子跳过。
    FormDepthExceeded,
    /// Form 退出时仍有未闭合的 `q`，仅丢弃子作用域的栈。
    ScopedGraphicsStateUnclosed,
    /// Form 退出时仍有操作数，仅丢弃子作用域的尾部。
    ScopedDanglingOperands,
}

/// 页级降级的原因（ADR-0013 §2）。降级页不产生 `PageRewrite`，
/// 增量写回因此逐字节保留它的原 content stream。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PageDegradeReason {
    /// content stream 的 token 语法无法有界恢复（未闭合字符串/数组/十六进制串等）。
    ContentStreamSyntax,
    /// 复合结构嵌套超过上限。
    NestingTooDeep,
    /// content stream 无法解码，或超过单流字节上限。
    ContentDecode,
    /// 页面框（MediaBox/CropBox）不可解析或退化。
    BadPageGeometry,
    /// `/Rotate` 不是 90 的整数倍。
    UnsupportedRotation,
    /// content stream 引用了资源字典里不存在的名字。
    MissingResource,
    /// Form XObject 的 `/BBox` 缺失或不可用。
    BadFormBBox,
    /// Form XObject 的 `/Matrix` 元素数不是 6 或含非数值。
    BadFormMatrix,
    /// XObject 不是流对象。
    XObjectNotAStream,
}

/// 汇总事件里的一条段级保留记录。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct PreservedParagraph {
    pub page_index: usize,
    pub paragraph_index: usize,
    pub reason: il::PreservedReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder_violation: Option<crate::translate::PlaceholderViolation>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct SuspiciousEchoParagraph {
    pub page_index: usize,
    pub paragraph_index: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "id", rename_all = "snake_case")]
pub enum Diagnostic {
    EngineBaselineMismatch {
        page_index: usize,
        character_index: usize,
        delta_x_pt: f64,
        delta_y_pt: f64,
    },
    EngineCharacterMismatch {
        page_index: usize,
        character_index: Option<usize>,
        walked_character_count: usize,
        engine_character_count: usize,
        walked_unicode: Option<char>,
        engine_unicode: Option<char>,
    },
    EngineCharacterAlignment {
        page_index: usize,
        walked_character_count: usize,
        engine_character_count: usize,
        extraction_equivalent_count: usize,
        explained_count: usize,
        strong_unicode_conflict_count: usize,
        weak_unicode_conflict_count: usize,
        unresolved_unicode_count: usize,
        walk_only_count: usize,
        engine_only_count: usize,
        residual_count: usize,
        #[serde(skip_serializing_if = "usize_is_zero")]
        baseline_residual_count: usize,
        #[serde(skip_serializing_if = "f64_is_zero")]
        baseline_residual_max_delta_x_pt: f64,
        #[serde(skip_serializing_if = "f64_is_zero")]
        baseline_residual_max_delta_y_pt: f64,
    },
    ScanSummary {
        scanned_page_indices: Vec<usize>,
        scanned_pages: usize,
        blank_pages: usize,
        content_pages: usize,
        total_pages: usize,
    },
    PageDegraded {
        page_index: usize,
        reason: PageDegradeReason,
    },
    ContentRecovered {
        page_index: usize,
        recovery: RecoveryKind,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        form_cycle_paths: Vec<Vec<u32>>,
    },
    TranslationRetry {
        page_index: usize,
        paragraph_index: usize,
        attempt: usize,
        delay_ms: u64,
        reason: RetryReason,
    },
    PlaceholderRetry {
        page_index: usize,
        paragraph_index: usize,
        attempt: usize,
        violation: crate::translate::PlaceholderViolation,
    },
    DegradationSummary {
        degraded_page_indices: Vec<usize>,
        degraded_pages: usize,
        preserved_paragraphs: Vec<PreservedParagraph>,
        preserved_paragraph_count: usize,
        suspicious_echoes: Vec<SuspiciousEchoParagraph>,
        suspicious_echo_count: usize,
        total_pages: usize,
    },
    TranslationIdentity {
        page_index: usize,
        paragraph_index: usize,
        request_characters: usize,
    },
    SuspiciousEcho {
        page_index: usize,
        paragraph_index: usize,
        request_characters: usize,
    },
    SuspiciousTranslationEchoRate {
        identity_count: usize,
        prose_paragraph_count: usize,
    },
    PlaceholderViolation {
        page_index: usize,
        paragraph_index: usize,
        violation: crate::translate::PlaceholderViolation,
    },
    TranslationFailureProfile {
        page_index: usize,
        paragraph_index: usize,
        response_bytes: usize,
        response_characters: usize,
        token_count: usize,
        token_scan_valid: bool,
    },
    MathPassthrough {
        page_index: usize,
        paragraph_index: usize,
        reading_order: usize,
        source_characters: usize,
    },
    UnsupportedOutputGlyph {
        page_index: usize,
        reading_order: usize,
        missing_characters: String,
        font_source: String,
        font_sha256: String,
        fallback_font_source: String,
        fallback_font_sha256: String,
    },
    SingleLineBoundsExpanded {
        page_index: usize,
        reading_order: usize,
        overflow_top_pt: f64,
        overflow_bottom_pt: f64,
    },
    MultiLineBoundsExpanded {
        page_index: usize,
        reading_order: usize,
        overflow_top_pt: f64,
        overflow_bottom_pt: f64,
    },
}

impl Diagnostic {
    #[must_use]
    pub const fn id(&self) -> DiagnosticId {
        match self {
            Self::EngineBaselineMismatch { .. } => DiagnosticId::EngineBaselineMismatch,
            Self::EngineCharacterMismatch { .. } => DiagnosticId::EngineCharacterMismatch,
            Self::EngineCharacterAlignment { .. } => DiagnosticId::EngineCharacterAlignment,
            Self::ScanSummary { .. } => DiagnosticId::ScanSummary,
            Self::PageDegraded { .. } => DiagnosticId::PageDegraded,
            Self::ContentRecovered { .. } => DiagnosticId::ContentRecovered,
            Self::TranslationRetry { .. } => DiagnosticId::TranslationRetry,
            Self::PlaceholderRetry { .. } => DiagnosticId::PlaceholderRetry,
            Self::DegradationSummary { .. } => DiagnosticId::DegradationSummary,
            Self::TranslationIdentity { .. } => DiagnosticId::TranslationIdentity,
            Self::SuspiciousEcho { .. } => DiagnosticId::SuspiciousEcho,
            Self::SuspiciousTranslationEchoRate { .. } => {
                DiagnosticId::SuspiciousTranslationEchoRate
            }
            Self::PlaceholderViolation { .. } => DiagnosticId::PlaceholderViolation,
            Self::TranslationFailureProfile { .. } => DiagnosticId::TranslationFailureProfile,
            Self::MathPassthrough { .. } => DiagnosticId::MathPassthrough,
            Self::UnsupportedOutputGlyph { .. } => DiagnosticId::UnsupportedOutputGlyph,
            Self::SingleLineBoundsExpanded { .. } => DiagnosticId::SingleLineBoundsExpanded,
            Self::MultiLineBoundsExpanded { .. } => DiagnosticId::MultiLineBoundsExpanded,
        }
    }

    #[must_use]
    const fn is_warning(&self) -> bool {
        !matches!(
            self,
            Self::TranslationIdentity { .. }
                | Self::TranslationFailureProfile { .. }
                | Self::MathPassthrough { .. }
                | Self::SingleLineBoundsExpanded { .. }
                | Self::MultiLineBoundsExpanded { .. }
        )
    }

    /// 汇总和页级降级无条件入库且不占普通诊断预算——逐字符/逐段明细可能被截断，
    /// 但页面是否降级及其总账必须完整到达消费者（ADR-0012 §5、ADR-0013 §5）。
    #[must_use]
    const fn bypasses_budget(&self) -> bool {
        matches!(
            self,
            Self::ScanSummary { .. } | Self::PageDegraded { .. } | Self::DegradationSummary { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "id", rename_all = "snake_case")]
pub enum DiagnosticEvent {
    EngineBaselineMismatch {
        page_index: usize,
        character_index: usize,
        delta_x_pt: f64,
        delta_y_pt: f64,
    },
    EngineCharacterMismatch {
        page_index: usize,
        character_index: Option<usize>,
        walked_character_count: usize,
        engine_character_count: usize,
        walked_unicode: Option<char>,
        engine_unicode: Option<char>,
    },
    EngineCharacterAlignment {
        page_index: usize,
        walked_character_count: usize,
        engine_character_count: usize,
        extraction_equivalent_count: usize,
        explained_count: usize,
        strong_unicode_conflict_count: usize,
        weak_unicode_conflict_count: usize,
        unresolved_unicode_count: usize,
        walk_only_count: usize,
        engine_only_count: usize,
        residual_count: usize,
        #[serde(skip_serializing_if = "usize_is_zero")]
        baseline_residual_count: usize,
        #[serde(skip_serializing_if = "f64_is_zero")]
        baseline_residual_max_delta_x_pt: f64,
        #[serde(skip_serializing_if = "f64_is_zero")]
        baseline_residual_max_delta_y_pt: f64,
    },
    ScanSummary {
        scanned_page_indices: Vec<usize>,
        scanned_pages: usize,
        blank_pages: usize,
        content_pages: usize,
        total_pages: usize,
    },
    PageDegraded {
        page_index: usize,
        reason: PageDegradeReason,
    },
    ContentRecovered {
        page_index: usize,
        recovery: RecoveryKind,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        form_cycle_paths: Vec<Vec<u32>>,
    },
    TranslationRetry {
        page_index: usize,
        paragraph_index: usize,
        attempt: usize,
        delay_ms: u64,
        reason: RetryReason,
    },
    PlaceholderRetry {
        page_index: usize,
        paragraph_index: usize,
        attempt: usize,
        violation: crate::translate::PlaceholderViolation,
    },
    DegradationSummary {
        degraded_page_indices: Vec<usize>,
        degraded_pages: usize,
        preserved_paragraphs: Vec<PreservedParagraph>,
        preserved_paragraph_count: usize,
        suspicious_echoes: Vec<SuspiciousEchoParagraph>,
        suspicious_echo_count: usize,
        total_pages: usize,
    },
    TranslationIdentity {
        page_index: usize,
        paragraph_index: usize,
        request_characters: usize,
    },
    SuspiciousEcho {
        page_index: usize,
        paragraph_index: usize,
        request_characters: usize,
    },
    SuspiciousTranslationEchoRate {
        identity_count: usize,
        prose_paragraph_count: usize,
    },
    PlaceholderViolation {
        page_index: usize,
        paragraph_index: usize,
        violation: crate::translate::PlaceholderViolation,
    },
    TranslationFailureProfile {
        page_index: usize,
        paragraph_index: usize,
        response_bytes: usize,
        response_characters: usize,
        token_count: usize,
        token_scan_valid: bool,
    },
    MathPassthrough {
        page_index: usize,
        paragraph_index: usize,
        reading_order: usize,
        source_characters: usize,
    },
    UnsupportedOutputGlyph {
        page_index: usize,
        reading_order: usize,
        missing_characters: String,
        font_source: String,
        font_sha256: String,
        fallback_font_source: String,
        fallback_font_sha256: String,
    },
    SingleLineBoundsExpanded {
        page_index: usize,
        reading_order: usize,
        overflow_top_pt: f64,
        overflow_bottom_pt: f64,
    },
    MultiLineBoundsExpanded {
        page_index: usize,
        reading_order: usize,
        overflow_top_pt: f64,
        overflow_bottom_pt: f64,
    },
    DroppedDiagnostics {
        count: usize,
        counts_by_id: Vec<DroppedDiagnosticCount>,
    },
}

impl DiagnosticEvent {
    #[must_use]
    pub const fn id(&self) -> DiagnosticId {
        match self {
            Self::EngineBaselineMismatch { .. } => DiagnosticId::EngineBaselineMismatch,
            Self::EngineCharacterMismatch { .. } => DiagnosticId::EngineCharacterMismatch,
            Self::EngineCharacterAlignment { .. } => DiagnosticId::EngineCharacterAlignment,
            Self::ScanSummary { .. } => DiagnosticId::ScanSummary,
            Self::PageDegraded { .. } => DiagnosticId::PageDegraded,
            Self::ContentRecovered { .. } => DiagnosticId::ContentRecovered,
            Self::TranslationRetry { .. } => DiagnosticId::TranslationRetry,
            Self::PlaceholderRetry { .. } => DiagnosticId::PlaceholderRetry,
            Self::DegradationSummary { .. } => DiagnosticId::DegradationSummary,
            Self::TranslationIdentity { .. } => DiagnosticId::TranslationIdentity,
            Self::SuspiciousEcho { .. } => DiagnosticId::SuspiciousEcho,
            Self::SuspiciousTranslationEchoRate { .. } => {
                DiagnosticId::SuspiciousTranslationEchoRate
            }
            Self::PlaceholderViolation { .. } => DiagnosticId::PlaceholderViolation,
            Self::TranslationFailureProfile { .. } => DiagnosticId::TranslationFailureProfile,
            Self::MathPassthrough { .. } => DiagnosticId::MathPassthrough,
            Self::UnsupportedOutputGlyph { .. } => DiagnosticId::UnsupportedOutputGlyph,
            Self::SingleLineBoundsExpanded { .. } => DiagnosticId::SingleLineBoundsExpanded,
            Self::MultiLineBoundsExpanded { .. } => DiagnosticId::MultiLineBoundsExpanded,
            Self::DroppedDiagnostics { .. } => DiagnosticId::DroppedDiagnostics,
        }
    }
}

impl From<&Diagnostic> for DiagnosticEvent {
    fn from(value: &Diagnostic) -> Self {
        match value {
            Diagnostic::EngineBaselineMismatch {
                page_index,
                character_index,
                delta_x_pt,
                delta_y_pt,
            } => Self::EngineBaselineMismatch {
                page_index: *page_index,
                character_index: *character_index,
                delta_x_pt: *delta_x_pt,
                delta_y_pt: *delta_y_pt,
            },
            Diagnostic::EngineCharacterMismatch {
                page_index,
                character_index,
                walked_character_count,
                engine_character_count,
                walked_unicode,
                engine_unicode,
            } => Self::EngineCharacterMismatch {
                page_index: *page_index,
                character_index: *character_index,
                walked_character_count: *walked_character_count,
                engine_character_count: *engine_character_count,
                walked_unicode: *walked_unicode,
                engine_unicode: *engine_unicode,
            },
            Diagnostic::EngineCharacterAlignment {
                page_index,
                walked_character_count,
                engine_character_count,
                extraction_equivalent_count,
                explained_count,
                strong_unicode_conflict_count,
                weak_unicode_conflict_count,
                unresolved_unicode_count,
                walk_only_count,
                engine_only_count,
                residual_count,
                baseline_residual_count,
                baseline_residual_max_delta_x_pt,
                baseline_residual_max_delta_y_pt,
            } => Self::EngineCharacterAlignment {
                page_index: *page_index,
                walked_character_count: *walked_character_count,
                engine_character_count: *engine_character_count,
                extraction_equivalent_count: *extraction_equivalent_count,
                explained_count: *explained_count,
                strong_unicode_conflict_count: *strong_unicode_conflict_count,
                weak_unicode_conflict_count: *weak_unicode_conflict_count,
                unresolved_unicode_count: *unresolved_unicode_count,
                walk_only_count: *walk_only_count,
                engine_only_count: *engine_only_count,
                residual_count: *residual_count,
                baseline_residual_count: *baseline_residual_count,
                baseline_residual_max_delta_x_pt: *baseline_residual_max_delta_x_pt,
                baseline_residual_max_delta_y_pt: *baseline_residual_max_delta_y_pt,
            },
            Diagnostic::ScanSummary {
                scanned_page_indices,
                scanned_pages,
                blank_pages,
                content_pages,
                total_pages,
            } => Self::ScanSummary {
                scanned_page_indices: scanned_page_indices.clone(),
                scanned_pages: *scanned_pages,
                blank_pages: *blank_pages,
                content_pages: *content_pages,
                total_pages: *total_pages,
            },
            Diagnostic::PageDegraded { page_index, reason } => Self::PageDegraded {
                page_index: *page_index,
                reason: *reason,
            },
            Diagnostic::ContentRecovered {
                page_index,
                recovery,
                form_cycle_paths,
            } => Self::ContentRecovered {
                page_index: *page_index,
                recovery: *recovery,
                form_cycle_paths: form_cycle_paths.clone(),
            },
            Diagnostic::TranslationRetry {
                page_index,
                paragraph_index,
                attempt,
                delay_ms,
                reason,
            } => Self::TranslationRetry {
                page_index: *page_index,
                paragraph_index: *paragraph_index,
                attempt: *attempt,
                delay_ms: *delay_ms,
                reason: *reason,
            },
            Diagnostic::PlaceholderRetry {
                page_index,
                paragraph_index,
                attempt,
                violation,
            } => Self::PlaceholderRetry {
                page_index: *page_index,
                paragraph_index: *paragraph_index,
                attempt: *attempt,
                violation: *violation,
            },
            Diagnostic::DegradationSummary {
                degraded_page_indices,
                degraded_pages,
                preserved_paragraphs,
                preserved_paragraph_count,
                suspicious_echoes,
                suspicious_echo_count,
                total_pages,
            } => Self::DegradationSummary {
                degraded_page_indices: degraded_page_indices.clone(),
                degraded_pages: *degraded_pages,
                preserved_paragraphs: preserved_paragraphs.clone(),
                preserved_paragraph_count: *preserved_paragraph_count,
                suspicious_echoes: suspicious_echoes.clone(),
                suspicious_echo_count: *suspicious_echo_count,
                total_pages: *total_pages,
            },
            Diagnostic::TranslationIdentity {
                page_index,
                paragraph_index,
                request_characters,
            } => Self::TranslationIdentity {
                page_index: *page_index,
                paragraph_index: *paragraph_index,
                request_characters: *request_characters,
            },
            Diagnostic::SuspiciousEcho {
                page_index,
                paragraph_index,
                request_characters,
            } => Self::SuspiciousEcho {
                page_index: *page_index,
                paragraph_index: *paragraph_index,
                request_characters: *request_characters,
            },
            Diagnostic::SuspiciousTranslationEchoRate {
                identity_count,
                prose_paragraph_count,
            } => Self::SuspiciousTranslationEchoRate {
                identity_count: *identity_count,
                prose_paragraph_count: *prose_paragraph_count,
            },
            Diagnostic::PlaceholderViolation {
                page_index,
                paragraph_index,
                violation,
            } => Self::PlaceholderViolation {
                page_index: *page_index,
                paragraph_index: *paragraph_index,
                violation: *violation,
            },
            Diagnostic::TranslationFailureProfile {
                page_index,
                paragraph_index,
                response_bytes,
                response_characters,
                token_count,
                token_scan_valid,
            } => Self::TranslationFailureProfile {
                page_index: *page_index,
                paragraph_index: *paragraph_index,
                response_bytes: *response_bytes,
                response_characters: *response_characters,
                token_count: *token_count,
                token_scan_valid: *token_scan_valid,
            },
            Diagnostic::MathPassthrough {
                page_index,
                paragraph_index,
                reading_order,
                source_characters,
            } => Self::MathPassthrough {
                page_index: *page_index,
                paragraph_index: *paragraph_index,
                reading_order: *reading_order,
                source_characters: *source_characters,
            },
            Diagnostic::UnsupportedOutputGlyph {
                page_index,
                reading_order,
                missing_characters,
                font_source,
                font_sha256,
                fallback_font_source,
                fallback_font_sha256,
            } => Self::UnsupportedOutputGlyph {
                page_index: *page_index,
                reading_order: *reading_order,
                missing_characters: missing_characters.clone(),
                font_source: font_source.clone(),
                font_sha256: font_sha256.clone(),
                fallback_font_source: fallback_font_source.clone(),
                fallback_font_sha256: fallback_font_sha256.clone(),
            },
            Diagnostic::SingleLineBoundsExpanded {
                page_index,
                reading_order,
                overflow_top_pt,
                overflow_bottom_pt,
            } => Self::SingleLineBoundsExpanded {
                page_index: *page_index,
                reading_order: *reading_order,
                overflow_top_pt: *overflow_top_pt,
                overflow_bottom_pt: *overflow_bottom_pt,
            },
            Diagnostic::MultiLineBoundsExpanded {
                page_index,
                reading_order,
                overflow_top_pt,
                overflow_bottom_pt,
            } => Self::MultiLineBoundsExpanded {
                page_index: *page_index,
                reading_order: *reading_order,
                overflow_top_pt: *overflow_top_pt,
                overflow_bottom_pt: *overflow_bottom_pt,
            },
        }
    }
}

#[derive(Debug, Default)]
pub struct Diagnostics {
    entries: Vec<Diagnostic>,
    debug_entries: Vec<Diagnostic>,
    dropped: usize,
    dropped_by_id: BTreeMap<DiagnosticId, usize>,
}

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        let bypasses_budget = diagnostic.bypasses_budget();
        let ordinary_count = self
            .entries
            .iter()
            .filter(|value| !value.bypasses_budget())
            .count();
        let same_id_count = self
            .entries
            .iter()
            .filter(|value| value.id() == diagnostic.id())
            .count();
        if bypasses_budget
            || (ordinary_count < MAX_DIAGNOSTICS && same_id_count < MAX_DIAGNOSTICS_PER_ID)
        {
            self.entries.push(diagnostic);
        } else {
            self.dropped += 1;
            *self.dropped_by_id.entry(diagnostic.id()).or_default() += 1;
        }
    }

    pub fn push_debug(&mut self, diagnostic: Diagnostic) {
        self.debug_entries.push(diagnostic);
    }

    #[must_use]
    pub fn entries(&self) -> &[Diagnostic] {
        &self.entries
    }

    #[must_use]
    pub const fn dropped(&self) -> usize {
        self.dropped
    }

    #[must_use]
    pub fn total_count(&self) -> usize {
        self.entries.len() + self.dropped
    }

    #[must_use]
    pub fn warning_count(&self) -> usize {
        let visible = self
            .entries
            .iter()
            .filter(|entry| entry.is_warning())
            .count();
        let dropped = self
            .dropped_by_id
            .iter()
            .filter(|(id, _)| {
                !matches!(
                    id,
                    DiagnosticId::TranslationIdentity
                        | DiagnosticId::TranslationFailureProfile
                        | DiagnosticId::MathPassthrough
                        | DiagnosticId::SingleLineBoundsExpanded
                        | DiagnosticId::MultiLineBoundsExpanded
                )
            })
            .map(|(_, count)| count)
            .sum::<usize>();
        visible + dropped
    }

    #[must_use]
    pub fn events(&self) -> Vec<DiagnosticEvent> {
        let mut events = self
            .entries
            .iter()
            .map(DiagnosticEvent::from)
            .collect::<Vec<_>>();
        if self.dropped > 0 {
            events.push(DiagnosticEvent::DroppedDiagnostics {
                count: self.dropped,
                counts_by_id: self
                    .dropped_by_id
                    .iter()
                    .map(|(id, count)| DroppedDiagnosticCount {
                        id: *id,
                        count: *count,
                    })
                    .collect(),
            });
        }
        events
    }

    #[must_use]
    pub fn debug_events(&self) -> Vec<DiagnosticEvent> {
        let mut events = self
            .entries
            .iter()
            .chain(&self.debug_entries)
            .map(DiagnosticEvent::from)
            .collect::<Vec<_>>();
        if self.dropped > 0 {
            events.push(DiagnosticEvent::DroppedDiagnostics {
                count: self.dropped,
                counts_by_id: self
                    .dropped_by_id
                    .iter()
                    .map(|(id, count)| DroppedDiagnosticCount {
                        id: *id,
                        count: *count,
                    })
                    .collect(),
            });
        }
        events
    }
}

pub fn serialize_line(event: &Event) -> Result<Vec<u8>> {
    let mut line = serde_json::to_vec(event).map_err(|error| {
        MimusError::internal(
            InternalReason::EventSerialization,
            format!("could not serialize protocol event: {error}"),
        )
    })?;
    line.push(b'\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use crate::error::InputReason;

    use super::*;

    fn baseline(index: usize) -> Diagnostic {
        Diagnostic::EngineBaselineMismatch {
            page_index: 0,
            character_index: index,
            delta_x_pt: 0.01,
            delta_y_pt: 0.0,
        }
    }

    #[test]
    fn diagnostics_are_bounded_and_summarized() {
        let mut diagnostics = Diagnostics::default();
        for index in 0..(MAX_DIAGNOSTICS + 7) {
            diagnostics.push(baseline(index));
        }
        let dropped = MAX_DIAGNOSTICS + 7 - MAX_DIAGNOSTICS_PER_ID;
        assert_eq!(diagnostics.entries().len(), MAX_DIAGNOSTICS_PER_ID);
        assert_eq!(diagnostics.dropped(), dropped);
        assert_eq!(diagnostics.total_count(), MAX_DIAGNOSTICS + 7);
        assert!(matches!(
            diagnostics.events().last(),
            Some(DiagnosticEvent::DroppedDiagnostics { count, .. }) if *count == dropped
        ));
    }

    #[test]
    fn one_diagnostic_kind_cannot_starve_other_kinds_or_page_degradation() {
        let mut diagnostics = Diagnostics::default();
        for index in 0..(MAX_DIAGNOSTICS + 7) {
            diagnostics.push(baseline(index));
        }
        diagnostics.push(Diagnostic::ContentRecovered {
            page_index: 1,
            recovery: RecoveryKind::UnknownOperator,
            form_cycle_paths: Vec::new(),
        });
        diagnostics.push(Diagnostic::PageDegraded {
            page_index: 2,
            reason: PageDegradeReason::ContentStreamSyntax,
        });

        assert_eq!(
            diagnostics
                .entries()
                .iter()
                .filter(|diagnostic| { diagnostic.id() == DiagnosticId::EngineBaselineMismatch })
                .count(),
            25
        );
        assert!(diagnostics.entries().iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::ContentRecovered {
                page_index: 1,
                recovery: RecoveryKind::UnknownOperator,
                ..
            }
        )));
        assert!(diagnostics.entries().iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::PageDegraded {
                page_index: 2,
                reason: PageDegradeReason::ContentStreamSyntax,
            }
        )));
    }

    #[test]
    fn dropped_diagnostics_report_counts_by_id() {
        let mut diagnostics = Diagnostics::default();
        for index in 0..30 {
            diagnostics.push(baseline(index));
        }
        for _ in 0..28 {
            diagnostics.push(Diagnostic::ContentRecovered {
                page_index: 1,
                recovery: RecoveryKind::UnknownOperator,
                form_cycle_paths: Vec::new(),
            });
        }

        let dropped = diagnostics.events().pop().unwrap();
        let value = serde_json::to_value(Event::new(EventKind::Diagnostic {
            diagnostic: dropped,
        }))
        .unwrap();
        assert_eq!(value["id"], "dropped_diagnostics");
        assert_eq!(value["count"], 8);
        assert_eq!(
            value["counts_by_id"],
            serde_json::json!([
                {"id": "engine_baseline_mismatch", "count": 5},
                {"id": "content_recovered", "count": 3}
            ])
        );
    }

    #[test]
    fn event_lines_use_cli_schema_two_and_end_with_one_newline() {
        let line = serialize_line(&Event::new(EventKind::Diagnostic {
            diagnostic: DiagnosticEvent::from(&baseline(0)),
        }))
        .unwrap();
        assert!(line.ends_with(b"\n"));
        assert!(!line[..line.len() - 1].contains(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["event"], "diagnostic");
        assert_eq!(value["id"], "engine_baseline_mismatch");
        assert_eq!(value["page_index"], 0);
        assert_eq!(value["character_index"], 0);
    }

    #[test]
    fn translation_retry_has_attempt_wait_and_typed_reason_without_source() {
        let event = DiagnosticEvent::from(&Diagnostic::TranslationRetry {
            page_index: 2,
            paragraph_index: 4,
            attempt: 3,
            delay_ms: 1_000,
            reason: RetryReason::RateLimited,
        });
        assert_eq!(event.id(), DiagnosticId::TranslationRetry);
        let value =
            serde_json::to_value(Event::new(EventKind::Diagnostic { diagnostic: event })).unwrap();
        assert_eq!(value["id"], "translation_retry");
        assert_eq!(value["page_index"], 2);
        assert_eq!(value["paragraph_index"], 4);
        assert_eq!(value["attempt"], 3);
        assert_eq!(value["delay_ms"], 1_000);
        assert_eq!(value["reason"], "rate_limited");
        assert!(value.get("source").is_none());
    }

    #[test]
    fn semantic_retry_and_suspicious_echo_have_paragraph_typed_wire_shapes() {
        let retry = DiagnosticEvent::from(&Diagnostic::PlaceholderRetry {
            page_index: 1,
            paragraph_index: 3,
            attempt: 1,
            violation: crate::translate::PlaceholderViolation::FormulaOrder,
        });
        let retry =
            serde_json::to_value(Event::new(EventKind::Diagnostic { diagnostic: retry })).unwrap();
        assert_eq!(retry["id"], "placeholder_retry");
        assert_eq!(retry["page_index"], 1);
        assert_eq!(retry["paragraph_index"], 3);
        assert_eq!(retry["attempt"], 1);
        assert_eq!(retry["violation"], "formula_order");

        let echo = DiagnosticEvent::from(&Diagnostic::SuspiciousEcho {
            page_index: 2,
            paragraph_index: 5,
            request_characters: 17,
        });
        let echo =
            serde_json::to_value(Event::new(EventKind::Diagnostic { diagnostic: echo })).unwrap();
        assert_eq!(echo["id"], "suspicious_echo");
        assert_eq!(echo["page_index"], 2);
        assert_eq!(echo["paragraph_index"], 5);
        assert_eq!(echo["request_characters"], 17);
    }

    #[test]
    fn recovered_character_mismatches_have_a_typed_additive_wire_shape() {
        let diagnostic = Diagnostic::EngineCharacterMismatch {
            page_index: 2,
            character_index: Some(4),
            walked_character_count: 7,
            engine_character_count: 7,
            walked_unicode: Some('M'),
            engine_unicode: None,
        };
        let event = DiagnosticEvent::from(&diagnostic);
        assert_eq!(event.id(), DiagnosticId::EngineCharacterMismatch);

        let line =
            serialize_line(&Event::new(EventKind::Diagnostic { diagnostic: event })).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["id"], "engine_character_mismatch");
        assert_eq!(value["page_index"], 2);
        assert_eq!(value["character_index"], 4);
        assert_eq!(value["walked_character_count"], 7);
        assert_eq!(value["engine_character_count"], 7);
        assert_eq!(value["walked_unicode"], "M");
        assert!(value["engine_unicode"].is_null());
    }

    #[test]
    fn classified_character_alignment_has_an_additive_wire_shape() {
        let diagnostic = Diagnostic::EngineCharacterAlignment {
            page_index: 2,
            walked_character_count: 10,
            engine_character_count: 9,
            extraction_equivalent_count: 1,
            explained_count: 8,
            strong_unicode_conflict_count: 2,
            weak_unicode_conflict_count: 3,
            unresolved_unicode_count: 4,
            walk_only_count: 5,
            engine_only_count: 6,
            residual_count: 7,
            baseline_residual_count: 8,
            baseline_residual_max_delta_x_pt: 0.125,
            baseline_residual_max_delta_y_pt: 0.25,
        };
        let event = DiagnosticEvent::from(&diagnostic);
        assert_eq!(event.id(), DiagnosticId::EngineCharacterAlignment);

        let line =
            serialize_line(&Event::new(EventKind::Diagnostic { diagnostic: event })).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["id"], "engine_character_alignment");
        assert_eq!(value["page_index"], 2);
        assert_eq!(value["walked_character_count"], 10);
        assert_eq!(value["engine_character_count"], 9);
        assert_eq!(value["extraction_equivalent_count"], 1);
        assert_eq!(value["explained_count"], 8);
        assert_eq!(value["strong_unicode_conflict_count"], 2);
        assert_eq!(value["weak_unicode_conflict_count"], 3);
        assert_eq!(value["unresolved_unicode_count"], 4);
        assert_eq!(value["walk_only_count"], 5);
        assert_eq!(value["engine_only_count"], 6);
        assert_eq!(value["residual_count"], 7);
        assert_eq!(value["baseline_residual_count"], 8);
        assert_eq!(value["baseline_residual_max_delta_x_pt"], 0.125);
        assert_eq!(value["baseline_residual_max_delta_y_pt"], 0.25);
    }

    #[test]
    fn missing_output_glyphs_have_a_typed_font_identity_wire_shape() {
        let value = serde_json::to_value(Event::new(EventKind::Diagnostic {
            diagnostic: DiagnosticEvent::from(&Diagnostic::UnsupportedOutputGlyph {
                page_index: 3,
                reading_order: 7,
                missing_characters: "龘".to_owned(),
                font_source: "flag:/tmp/font.ttf".to_owned(),
                font_sha256: "abc123".to_owned(),
                fallback_font_source: "flag:/tmp/fallback.ttf".to_owned(),
                fallback_font_sha256: "def456".to_owned(),
            }),
        }))
        .unwrap();
        assert_eq!(value["id"], "unsupported_output_glyph");
        assert_eq!(value["page_index"], 3);
        assert_eq!(value["reading_order"], 7);
        assert_eq!(value["missing_characters"], "龘");
        assert_eq!(value["font_source"], "flag:/tmp/font.ttf");
        assert_eq!(value["font_sha256"], "abc123");
        assert_eq!(value["fallback_font_source"], "flag:/tmp/fallback.ttf");
        assert_eq!(value["fallback_font_sha256"], "def456");
    }

    #[test]
    fn recursive_form_recovery_exposes_indirect_object_paths() {
        let event = DiagnosticEvent::from(&Diagnostic::ContentRecovered {
            page_index: 0,
            recovery: RecoveryKind::MutuallyRecursiveForm,
            form_cycle_paths: vec![vec![12, 13, 12]],
        });
        let line =
            serialize_line(&Event::new(EventKind::Diagnostic { diagnostic: event })).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();

        assert_eq!(value["id"], "content_recovered");
        assert_eq!(value["recovery"], "mutually_recursive_form");
        assert_eq!(value["form_cycle_paths"], serde_json::json!([[12, 13, 12]]));

        let event = DiagnosticEvent::from(&Diagnostic::ContentRecovered {
            page_index: 0,
            recovery: RecoveryKind::UnknownOperator,
            form_cycle_paths: Vec::new(),
        });
        let value =
            serde_json::to_value(Event::new(EventKind::Diagnostic { diagnostic: event })).unwrap();
        assert!(value.get("form_cycle_paths").is_none());
    }

    #[test]
    fn scan_summary_bypasses_the_normal_diagnostic_limit_but_counts_once() {
        let mut diagnostics = Diagnostics::default();
        for index in 0..(MAX_DIAGNOSTICS + 7) {
            diagnostics.push(baseline(index));
        }
        diagnostics.push(Diagnostic::ScanSummary {
            scanned_page_indices: vec![1, 3],
            scanned_pages: 2,
            blank_pages: 4,
            content_pages: 5,
            total_pages: 9,
        });

        let dropped = MAX_DIAGNOSTICS + 7 - MAX_DIAGNOSTICS_PER_ID;
        assert_eq!(diagnostics.entries().len(), MAX_DIAGNOSTICS_PER_ID + 1);
        assert_eq!(diagnostics.dropped(), dropped);
        assert_eq!(diagnostics.total_count(), MAX_DIAGNOSTICS + 8);
        assert!(diagnostics.events().iter().any(|event| matches!(
            event,
            DiagnosticEvent::ScanSummary {
                scanned_page_indices,
                scanned_pages: 2,
                blank_pages: 4,
                content_pages: 5,
                total_pages: 9,
            } if scanned_page_indices == &[1, 3]
        )));
    }

    #[test]
    fn degradation_summary_shares_the_scan_summary_privilege() {
        let mut diagnostics = Diagnostics::default();
        for index in 0..(MAX_DIAGNOSTICS + 3) {
            diagnostics.push(baseline(index));
        }
        diagnostics.push(Diagnostic::PageDegraded {
            page_index: 2,
            reason: PageDegradeReason::ContentStreamSyntax,
        });
        diagnostics.push(Diagnostic::DegradationSummary {
            degraded_page_indices: vec![2],
            degraded_pages: 1,
            preserved_paragraphs: vec![PreservedParagraph {
                page_index: 4,
                paragraph_index: 0,
                reason: il::PreservedReason::UnreliableUnicode,
                placeholder_violation: None,
            }],
            preserved_paragraph_count: 1,
            suspicious_echoes: Vec::new(),
            suspicious_echo_count: 0,
            total_pages: 6,
        });

        // 逐页降级与汇总都不吃普通诊断预算，必须完整到达。
        let dropped = MAX_DIAGNOSTICS + 3 - MAX_DIAGNOSTICS_PER_ID;
        assert_eq!(diagnostics.entries().len(), MAX_DIAGNOSTICS_PER_ID + 2);
        assert_eq!(diagnostics.dropped(), dropped);
        let events = diagnostics.events();
        assert!(events.iter().any(|event| matches!(
            event,
            DiagnosticEvent::PageDegraded {
                page_index: 2,
                reason: PageDegradeReason::ContentStreamSyntax,
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            DiagnosticEvent::DegradationSummary {
                degraded_page_indices,
                degraded_pages: 1,
                total_pages: 6,
                preserved_paragraphs,
                preserved_paragraph_count: 1,
                ..
            } if degraded_page_indices == &[2] && preserved_paragraphs.len() == 1
        )));
        assert!(matches!(
            events.last(),
            Some(DiagnosticEvent::DroppedDiagnostics { count, .. }) if *count == dropped
        ));
    }

    #[test]
    fn degradation_diagnostics_serialize_with_snake_case_reasons() {
        let line = serialize_line(&Event::new(EventKind::Diagnostic {
            diagnostic: DiagnosticEvent::from(&Diagnostic::PageDegraded {
                page_index: 3,
                reason: PageDegradeReason::UnsupportedRotation,
            }),
        }))
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["event"], "diagnostic");
        assert_eq!(value["id"], "page_degraded");
        assert_eq!(value["page_index"], 3);
        assert_eq!(value["reason"], "unsupported_rotation");
    }

    #[test]
    fn only_scanned_pdf_errors_serialize_scan_counts() {
        let scanned = MimusError::input(InputReason::ScannedPdf, "scanned").with_scan_counts(4, 5);
        let scanned = serde_json::to_value(Event::new(EventKind::from_error(&scanned))).unwrap();
        assert_eq!(scanned["scanned_pages"], 4);
        assert_eq!(scanned["total_pages"], 5);

        let unsupported =
            MimusError::input(InputReason::UnsupportedPdf, "unsupported").with_scan_counts(4, 5);
        let unsupported =
            serde_json::to_value(Event::new(EventKind::from_error(&unsupported))).unwrap();
        assert!(unsupported.get("scanned_pages").is_none());
        assert!(unsupported.get("total_pages").is_none());
    }

    #[test]
    fn translation_identity_is_informational_while_suspicious_echo_rate_is_a_warning() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.push(Diagnostic::TranslationIdentity {
            page_index: 0,
            paragraph_index: 2,
            request_characters: 18,
        });
        diagnostics.push(Diagnostic::SuspiciousTranslationEchoRate {
            identity_count: 3,
            prose_paragraph_count: 4,
        });

        assert_eq!(diagnostics.total_count(), 2);
        assert_eq!(diagnostics.warning_count(), 1);
        assert!(matches!(
            diagnostics.events().as_slice(),
            [
                DiagnosticEvent::TranslationIdentity { .. },
                DiagnosticEvent::SuspiciousTranslationEchoRate { .. }
            ]
        ));
    }

    #[test]
    fn math_passthrough_is_informational_typed_and_budgeted_per_id() {
        let mut diagnostics = Diagnostics::default();
        for reading_order in 0..(MAX_DIAGNOSTICS_PER_ID + 3) {
            diagnostics.push(Diagnostic::MathPassthrough {
                page_index: 1,
                paragraph_index: 2,
                reading_order,
                source_characters: 7,
            });
        }

        assert_eq!(diagnostics.entries().len(), MAX_DIAGNOSTICS_PER_ID);
        assert_eq!(diagnostics.dropped(), 3);
        assert_eq!(diagnostics.warning_count(), 0);
        let first = serde_json::to_value(Event::new(EventKind::Diagnostic {
            diagnostic: DiagnosticEvent::from(&diagnostics.entries()[0]),
        }))
        .unwrap();
        assert_eq!(first["id"], "math_passthrough");
        assert_eq!(first["page_index"], 1);
        assert_eq!(first["paragraph_index"], 2);
        assert_eq!(first["reading_order"], 0);
        assert_eq!(first["source_characters"], 7);
        assert!(matches!(
            diagnostics.events().last(),
            Some(DiagnosticEvent::DroppedDiagnostics {
                count: 3,
                counts_by_id,
            }) if counts_by_id == &[DroppedDiagnosticCount {
                id: DiagnosticId::MathPassthrough,
                count: 3,
            }]
        ));
    }

    #[test]
    fn placeholder_subtypes_and_failure_profiles_have_redacted_wire_shapes() {
        let violation = Event::new(EventKind::Diagnostic {
            diagnostic: DiagnosticEvent::from(&Diagnostic::PlaceholderViolation {
                page_index: 1,
                paragraph_index: 3,
                violation: crate::translate::PlaceholderViolation::FormulaOrder,
            }),
        });
        let violation = String::from_utf8(serialize_line(&violation).unwrap()).unwrap();
        assert!(violation.contains("\"id\":\"placeholder_violation\""));
        assert!(violation.contains("\"violation\":\"formula_order\""));

        let canary = "response-secret-canary";
        let profile = Event::new(EventKind::Diagnostic {
            diagnostic: DiagnosticEvent::from(&Diagnostic::TranslationFailureProfile {
                page_index: 1,
                paragraph_index: 3,
                response_bytes: canary.len(),
                response_characters: canary.chars().count(),
                token_count: 2,
                token_scan_valid: false,
            }),
        });
        let profile = String::from_utf8(serialize_line(&profile).unwrap()).unwrap();
        assert!(profile.contains("\"id\":\"translation_failure_profile\""));
        assert!(profile.contains("\"response_characters\":22"));
        assert!(!profile.contains(canary));
        assert!(!profile.contains("response_text"));
    }

    #[test]
    fn failure_profiles_are_debug_only() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.push(Diagnostic::PlaceholderViolation {
            page_index: 1,
            paragraph_index: 3,
            violation: crate::translate::PlaceholderViolation::PartialToken,
        });
        diagnostics.push_debug(Diagnostic::TranslationFailureProfile {
            page_index: 1,
            paragraph_index: 3,
            response_bytes: 23,
            response_characters: 23,
            token_count: 1,
            token_scan_valid: false,
        });

        assert!(matches!(
            diagnostics.events().as_slice(),
            [DiagnosticEvent::PlaceholderViolation { .. }]
        ));
        assert!(matches!(
            diagnostics.debug_events().as_slice(),
            [
                DiagnosticEvent::PlaceholderViolation { .. },
                DiagnosticEvent::TranslationFailureProfile { .. }
            ]
        ));
    }
}
