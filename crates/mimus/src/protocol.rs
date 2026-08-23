use std::io::{ErrorKind, Write};
use std::process::ExitCode;
use std::sync::Mutex;

use mimus_core::error::{ErrorReason, InternalReason, IoReason, MimusError, Result};
use mimus_core::event::{
    DiagnosticEvent, Event, EventKind, EventSink, MINIMAL_SERIALIZATION_ERROR_LINE, ResultPayload,
    Stage, serialize_line,
};

type Serializer = fn(&Event) -> Result<Vec<u8>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputState {
    Open,
    ClosedByEpipe,
    Poisoned,
    Terminal,
}

struct SessionState<W> {
    writer: W,
    output: OutputState,
}

pub(crate) struct ProtocolSession<W = std::io::Stdout> {
    json: bool,
    serializer: Serializer,
    state: Mutex<SessionState<W>>,
}

impl ProtocolSession<std::io::Stdout> {
    pub(crate) fn stdout(json: bool) -> Self {
        Self::with_writer(std::io::stdout(), json, serialize_line)
    }
}

impl<W: Write + Send> ProtocolSession<W> {
    pub(crate) fn with_writer(writer: W, json: bool, serializer: Serializer) -> Self {
        Self {
            json,
            serializer,
            state: Mutex::new(SessionState {
                writer,
                output: OutputState::Open,
            }),
        }
    }

    pub(crate) fn emit_diagnostics(&self, diagnostics: &[DiagnosticEvent]) -> Result<()> {
        for diagnostic in diagnostics {
            self.emit(Event::new(EventKind::Diagnostic {
                diagnostic: diagnostic.clone(),
            }))?;
        }
        Ok(())
    }

    pub(crate) fn finish_result(
        &self,
        result: ResultPayload,
        pages: usize,
        warnings: usize,
    ) -> ExitCode {
        match self.emit(Event::new(EventKind::Result {
            result,
            pages,
            warnings,
        })) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                report_output_failure(&error);
                ExitCode::from(error.category().code())
            }
        }
    }

    pub(crate) fn finish_error(&self, error: MimusError) -> ExitCode {
        let code = error.category().code();
        if matches!(
            error.reason(),
            ErrorReason::Internal(InternalReason::EventSerialization)
        ) {
            if self.output_state() == OutputState::Poisoned {
                report_output_failure(&error);
            }
            return ExitCode::from(code);
        }

        match self.emit(Event::new(EventKind::from_error(&error))) {
            Ok(()) => ExitCode::from(code),
            Err(output_error) => {
                report_output_failure(&output_error);
                ExitCode::from(output_error.category().code())
            }
        }
    }

    fn output_state(&self) -> OutputState {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .output
    }

    fn emit_json(&self, event: &Event, state: &mut SessionState<W>) -> Result<()> {
        let line = match (self.serializer)(event) {
            Ok(line) => line,
            Err(error) => {
                match write_bytes(state, MINIMAL_SERIALIZATION_ERROR_LINE) {
                    Ok(()) if state.output == OutputState::Open => {
                        state.output = OutputState::Terminal;
                    }
                    Ok(()) => {}
                    Err(_) => {}
                }
                return Err(error);
            }
        };
        write_bytes(state, &line)?;
        if event.kind.is_terminal() && state.output == OutputState::Open {
            state.output = OutputState::Terminal;
        }
        Ok(())
    }

    fn emit_human(&self, event: Event, state: &mut SessionState<W>) -> Result<()> {
        match event.kind {
            EventKind::StageStarted { stage } => {
                let _ = writeln!(std::io::stderr().lock(), "{}...", human_stage_name(stage));
            }
            EventKind::PageProgress {
                stage,
                page_index,
                total_pages,
            } => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: page {}/{total_pages}",
                    human_stage_name(stage),
                    page_index + 1
                );
            }
            EventKind::StageFinished { .. } => {}
            EventKind::Diagnostic { diagnostic } => render_diagnostic(diagnostic),
            EventKind::Result { result, .. } => {
                let bytes = match result {
                    ResultPayload::Translate { output } => format!("{output}\n").into_bytes(),
                    ResultPayload::Inspect { il } => mimus_core::il::canonical_json(&il)?,
                };
                write_bytes(state, &bytes)?;
                if state.output == OutputState::Open {
                    state.output = OutputState::Terminal;
                }
            }
            EventKind::Error {
                reason,
                message,
                hint,
                ..
            } => {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "error[{reason}]: {message}");
                if let Some(hint) = hint {
                    let _ = writeln!(stderr, "hint: {hint}");
                }
                state.output = OutputState::Terminal;
            }
        }
        Ok(())
    }
}

impl<W: Write + Send> EventSink for ProtocolSession<W> {
    fn emit(&self, event: Event) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match state.output {
            OutputState::ClosedByEpipe | OutputState::Terminal => return Ok(()),
            OutputState::Poisoned => return Err(stdout_write_error("stdout is poisoned")),
            OutputState::Open => {}
        }
        if self.json {
            self.emit_json(&event, &mut state)
        } else {
            self.emit_human(event, &mut state)
        }
    }
}

fn write_bytes<W: Write>(state: &mut SessionState<W>, bytes: &[u8]) -> Result<()> {
    match state.writer.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => {
            state.output = OutputState::ClosedByEpipe;
            Ok(())
        }
        Err(error) => {
            state.output = OutputState::Poisoned;
            Err(stdout_write_error(format!(
                "could not write stdout: {error}"
            )))
        }
    }
}

fn stdout_write_error(message: impl Into<String>) -> MimusError {
    MimusError::io(IoReason::StdoutWrite, message)
}

fn report_output_failure(error: &MimusError) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "error[{}]: {}",
        error.reason(),
        error
    );
}

fn render_diagnostic(diagnostic: DiagnosticEvent) {
    match diagnostic {
        DiagnosticEvent::EngineBaselineMismatch {
            page_index,
            character_index,
            delta_x_pt,
            delta_y_pt,
        } => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning[engine_baseline_mismatch]: page {} character {character_index} delta=({delta_x_pt:.6},{delta_y_pt:.6})pt",
                page_index + 1
            );
        }
        DiagnosticEvent::DroppedDiagnostics { count } => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning[dropped_diagnostics]: {count} additional diagnostics dropped"
            );
        }
    }
}

const fn human_stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Parse => "parse",
        Stage::ScanDetect => "scan detect",
        Stage::Layout => "layout",
        Stage::ParagraphFind => "paragraph find",
        Stage::StylesAndFormulas => "styles and formulas",
        Stage::ExtractTerms => "extract terms",
        Stage::Translate => "translate",
        Stage::Typeset => "typeset",
        Stage::FontEmbed => "font embed",
        Stage::Write => "write",
    }
}
