use std::sync::Mutex;

use serde::Serialize;

use crate::error::{ExitCategory, InternalReason, MimusError, Result};
use crate::il;

// CONTEXT "双 schema_version": CLI 事件协议与 IL 的演进节奏不同，禁止共用版本号。
pub const SCHEMA_VERSION: u32 = 2;
pub const MAX_DIAGNOSTICS: usize = 100;
pub const MINIMAL_SERIALIZATION_ERROR_LINE: &[u8] = b"{\"schema_version\":2,\"event\":\"error\",\"category\":\"internal\",\"reason\":\"event_serialization\",\"message\":\"could not serialize protocol event\",\"hint\":null}\n";

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
    Translate { output: String },
    Inspect { il: il::Document },
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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticId {
    EngineBaselineMismatch,
    ScanSummary,
    PageDegraded,
    DegradationSummary,
    DroppedDiagnostics,
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
    DegradationSummary {
        degraded_page_indices: Vec<usize>,
        degraded_pages: usize,
        preserved_paragraphs: Vec<PreservedParagraph>,
        total_pages: usize,
    },
}

impl Diagnostic {
    #[must_use]
    pub const fn id(&self) -> DiagnosticId {
        match self {
            Self::EngineBaselineMismatch { .. } => DiagnosticId::EngineBaselineMismatch,
            Self::ScanSummary { .. } => DiagnosticId::ScanSummary,
            Self::PageDegraded { .. } => DiagnosticId::PageDegraded,
            Self::DegradationSummary { .. } => DiagnosticId::DegradationSummary,
        }
    }

    /// 汇总类诊断无条件入库且不占 `MAX_DIAGNOSTICS` 名额——逐页/逐段的明细可能被
    /// 截断，但「哪些页被降级」这个总账必须完整到达消费者（ADR-0012 §5、ADR-0013 §5）。
    #[must_use]
    const fn is_summary(&self) -> bool {
        matches!(
            self,
            Self::ScanSummary { .. } | Self::DegradationSummary { .. }
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
    DegradationSummary {
        degraded_page_indices: Vec<usize>,
        degraded_pages: usize,
        preserved_paragraphs: Vec<PreservedParagraph>,
        total_pages: usize,
    },
    DroppedDiagnostics {
        count: usize,
    },
}

impl DiagnosticEvent {
    #[must_use]
    pub const fn id(&self) -> DiagnosticId {
        match self {
            Self::EngineBaselineMismatch { .. } => DiagnosticId::EngineBaselineMismatch,
            Self::ScanSummary { .. } => DiagnosticId::ScanSummary,
            Self::PageDegraded { .. } => DiagnosticId::PageDegraded,
            Self::DegradationSummary { .. } => DiagnosticId::DegradationSummary,
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
            Diagnostic::DegradationSummary {
                degraded_page_indices,
                degraded_pages,
                preserved_paragraphs,
                total_pages,
            } => Self::DegradationSummary {
                degraded_page_indices: degraded_page_indices.clone(),
                degraded_pages: *degraded_pages,
                preserved_paragraphs: preserved_paragraphs.clone(),
                total_pages: *total_pages,
            },
        }
    }
}

#[derive(Debug, Default)]
pub struct Diagnostics {
    entries: Vec<Diagnostic>,
    dropped: usize,
}

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        if diagnostic.is_summary()
            || self
                .entries
                .iter()
                .filter(|value| !value.is_summary())
                .count()
                < MAX_DIAGNOSTICS
        {
            self.entries.push(diagnostic);
        } else {
            self.dropped += 1;
        }
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
    pub fn events(&self) -> Vec<DiagnosticEvent> {
        let mut events = self
            .entries
            .iter()
            .map(DiagnosticEvent::from)
            .collect::<Vec<_>>();
        if self.dropped > 0 {
            events.push(DiagnosticEvent::DroppedDiagnostics {
                count: self.dropped,
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
        assert_eq!(diagnostics.entries().len(), MAX_DIAGNOSTICS);
        assert_eq!(diagnostics.dropped(), 7);
        assert_eq!(diagnostics.total_count(), MAX_DIAGNOSTICS + 7);
        assert!(matches!(
            diagnostics.events().last(),
            Some(DiagnosticEvent::DroppedDiagnostics { count: 7 })
        ));
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

        assert_eq!(diagnostics.entries().len(), MAX_DIAGNOSTICS + 1);
        assert_eq!(diagnostics.dropped(), 7);
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
            }],
            total_pages: 6,
        });

        // 逐页明细吃上限（被丢弃），汇总仍然完整到达。
        assert_eq!(diagnostics.entries().len(), MAX_DIAGNOSTICS + 1);
        assert_eq!(diagnostics.dropped(), 4);
        let events = diagnostics.events();
        assert!(events.iter().any(|event| matches!(
            event,
            DiagnosticEvent::DegradationSummary {
                degraded_page_indices,
                degraded_pages: 1,
                total_pages: 6,
                preserved_paragraphs,
            } if degraded_page_indices == &[2] && preserved_paragraphs.len() == 1
        )));
        assert!(matches!(
            events.last(),
            Some(DiagnosticEvent::DroppedDiagnostics { count: 4 })
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
}
