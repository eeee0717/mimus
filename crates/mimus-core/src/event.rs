use std::sync::Mutex;

use serde::Serialize;

use crate::error::{ErrorReason, MimusError};

// CONTEXT "双 schema_version": CLI 事件协议与 IL 的演进节奏不同，禁止共用版本号。
pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_DIAGNOSTICS: usize = 100;

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
        page: usize,
        total_pages: usize,
    },
    Result {
        output: String,
        pages: usize,
        warnings: usize,
    },
    Error {
        reason: ErrorReason,
        message: String,
        hint: Option<String>,
    },
}

impl EventKind {
    #[must_use]
    pub fn from_error(error: &MimusError) -> Self {
        Self::Error {
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

pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

#[derive(Debug, Default)]
pub struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn emit(&self, _event: Event) {}
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
    fn emit(&self, event: Event) {
        self.events
            .lock()
            .expect("event mutex poisoned")
            .push(event);
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticId {
    EngineBaselineMismatch,
    DroppedDiagnostics,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub id: DiagnosticId,
    pub detail: String,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_are_bounded() {
        let mut diagnostics = Diagnostics::default();
        for index in 0..(MAX_DIAGNOSTICS + 7) {
            diagnostics.push(Diagnostic {
                id: DiagnosticId::EngineBaselineMismatch,
                detail: index.to_string(),
            });
        }
        assert_eq!(diagnostics.entries().len(), MAX_DIAGNOSTICS);
        assert_eq!(diagnostics.dropped(), 7);
    }
}
