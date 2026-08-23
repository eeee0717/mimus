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
    },
}

impl EventKind {
    #[must_use]
    pub fn from_error(error: &MimusError) -> Self {
        Self::Error {
            category: error.category(),
            reason: error.reason(),
            message: error.to_string(),
            hint: error.hint().map(str::to_owned),
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
    DroppedDiagnostics,
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
}

impl Diagnostic {
    #[must_use]
    pub const fn id(&self) -> DiagnosticId {
        match self {
            Self::EngineBaselineMismatch { .. } => DiagnosticId::EngineBaselineMismatch,
        }
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
    DroppedDiagnostics {
        count: usize,
    },
}

impl DiagnosticEvent {
    #[must_use]
    pub const fn id(&self) -> DiagnosticId {
        match self {
            Self::EngineBaselineMismatch { .. } => DiagnosticId::EngineBaselineMismatch,
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
        if self.entries.len() < MAX_DIAGNOSTICS {
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
}
