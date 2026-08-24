use std::io::{ErrorKind, Write};
use std::process::ExitCode;
use std::sync::Mutex;

use mimus_core::error::{ErrorReason, InternalReason, IoReason, MimusError, Result};
use mimus_core::event::{
    DiagnosticEvent, Event, EventKind, EventSink, MINIMAL_SERIALIZATION_ERROR_LINE,
    PageDegradeReason, RecoveryKind, ResultPayload, Stage, serialize_line,
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
    match state.writer.write(bytes) {
        Ok(written) if written == bytes.len() => Ok(()),
        Ok(written) => {
            state.output = OutputState::Poisoned;
            Err(stdout_write_error(format!(
                "stdout wrote {written} of {} bytes",
                bytes.len()
            )))
        }
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
        DiagnosticEvent::ScanSummary {
            scanned_page_indices,
            scanned_pages,
            content_pages,
            ..
        } => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning[scan_summary]: {scanned_pages} of {content_pages} content pages are scanned; page indices: {scanned_page_indices:?}"
            );
        }
        DiagnosticEvent::PageDegraded { page_index, reason } => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning[page_degraded]: page {} kept as-is ({})",
                page_index + 1,
                human_page_degrade_reason(reason)
            );
        }
        DiagnosticEvent::ContentRecovered {
            page_index,
            recovery,
        } => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning[content_recovered]: page {} translated after recovering from malformed content ({})",
                page_index + 1,
                human_recovery_kind(recovery)
            );
        }
        DiagnosticEvent::DegradationSummary {
            degraded_page_indices,
            degraded_pages,
            preserved_paragraphs,
            total_pages,
        } => {
            let pages = degraded_page_indices
                .iter()
                .map(|index| (index + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning[degradation_summary]: {degraded_pages} of {total_pages} pages kept as-is [{pages}]; {} paragraphs kept as source text",
                preserved_paragraphs.len()
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

const fn human_recovery_kind(recovery: RecoveryKind) -> &'static str {
    match recovery {
        RecoveryKind::ImplicitTextObject => "text operators outside BT/ET",
        RecoveryKind::SkippedTjElement => "an illegal element inside a TJ array",
    }
}

const fn human_page_degrade_reason(reason: PageDegradeReason) -> &'static str {
    match reason {
        PageDegradeReason::ContentStreamSyntax => "content stream syntax could not be recovered",
        PageDegradeReason::NestingTooDeep => "nesting exceeded the supported depth",
        PageDegradeReason::ContentDecode => "content stream could not be decoded",
        PageDegradeReason::BadPageGeometry => "page boxes could not be parsed",
        PageDegradeReason::UnsupportedRotation => "its /Rotate is not supported yet",
        PageDegradeReason::MissingResource => "a referenced resource is missing",
        PageDegradeReason::BadFormBBox => "a form XObject has an unusable /BBox",
        PageDegradeReason::BadFormMatrix => "a form XObject has an unusable /Matrix",
        PageDegradeReason::XObjectNotAStream => "an XObject is not a stream",
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::Arc;

    use mimus_core::error::{InputReason, InternalReason};

    use super::*;

    #[derive(Debug, Clone)]
    enum WriteStep {
        All,
        Bytes(usize),
        Fail(ErrorKind),
    }

    #[derive(Clone)]
    struct ScriptedWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
        steps: Arc<Mutex<VecDeque<WriteStep>>>,
    }

    impl ScriptedWriter {
        fn new(steps: impl IntoIterator<Item = WriteStep>) -> (Self, Arc<Mutex<Vec<u8>>>) {
            let bytes = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    bytes: Arc::clone(&bytes),
                    steps: Arc::new(Mutex::new(steps.into_iter().collect())),
                },
                bytes,
            )
        }
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            match self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(WriteStep::All)
            {
                WriteStep::All => {
                    self.bytes.lock().unwrap().extend_from_slice(buffer);
                    Ok(buffer.len())
                }
                WriteStep::Bytes(count) => {
                    let count = count.min(buffer.len());
                    self.bytes
                        .lock()
                        .unwrap()
                        .extend_from_slice(&buffer[..count]);
                    Ok(count)
                }
                WriteStep::Fail(kind) => Err(io::Error::new(kind, "injected stdout failure")),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn started() -> Event {
        Event::new(EventKind::StageStarted {
            stage: Stage::Parse,
        })
    }

    fn result() -> ResultPayload {
        ResultPayload::Translate {
            output: "paper.zh.pdf".to_owned(),
        }
    }

    fn input_error() -> MimusError {
        MimusError::input(InputReason::UnsupportedPdf, "unsupported input")
    }

    fn bytes(value: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
        value.lock().unwrap().clone()
    }

    fn failing_serializer(_event: &Event) -> Result<Vec<u8>> {
        Err(MimusError::internal(
            InternalReason::EventSerialization,
            "injected serialization failure",
        ))
    }

    #[test]
    fn epipe_closes_stdout_but_preserves_the_business_exit_code() {
        let (writer, output) = ScriptedWriter::new([WriteStep::Fail(ErrorKind::BrokenPipe)]);
        let session = ProtocolSession::with_writer(writer, true, serialize_line);

        session.emit(started()).unwrap();
        assert_eq!(session.output_state(), OutputState::ClosedByEpipe);
        assert_eq!(session.finish_error(input_error()), ExitCode::from(2));
        assert!(bytes(&output).is_empty());

        let (writer, _) = ScriptedWriter::new([WriteStep::Fail(ErrorKind::BrokenPipe)]);
        let session = ProtocolSession::with_writer(writer, true, serialize_line);
        session.emit(started()).unwrap();
        assert_eq!(session.finish_result(result(), 1, 0), ExitCode::SUCCESS);
    }

    #[test]
    fn zero_byte_write_poisons_stdout_and_prevents_all_later_writes() {
        let (writer, output) = ScriptedWriter::new([WriteStep::Bytes(0)]);
        let session = ProtocolSession::with_writer(writer, true, serialize_line);

        let error = session.emit(started()).unwrap_err();
        assert_eq!(error.reason(), ErrorReason::Io(IoReason::StdoutWrite));
        assert_eq!(session.output_state(), OutputState::Poisoned);
        assert!(bytes(&output).is_empty());
        assert!(session.emit(started()).is_err());
        assert_eq!(session.finish_error(input_error()), ExitCode::from(5));
        assert!(bytes(&output).is_empty());
    }

    #[test]
    fn partial_json_write_poisons_stdout_without_appending_another_event() {
        let line = serialize_line(&started()).unwrap();
        let split = line.len() / 2;
        let (writer, output) = ScriptedWriter::new([WriteStep::Bytes(split), WriteStep::All]);
        let session = ProtocolSession::with_writer(writer, true, serialize_line);

        assert!(session.emit(started()).is_err());
        assert_eq!(bytes(&output), line[..split]);
        assert!(session.emit(started()).is_err());
        assert_eq!(bytes(&output), line[..split]);
    }

    #[test]
    fn newline_write_failure_leaves_one_unterminated_line_and_no_followup() {
        let line = serialize_line(&started()).unwrap();
        let (writer, output) =
            ScriptedWriter::new([WriteStep::Bytes(line.len() - 1), WriteStep::All]);
        let session = ProtocolSession::with_writer(writer, true, serialize_line);

        assert!(session.emit(started()).is_err());
        assert_eq!(bytes(&output), line[..line.len() - 1]);
        assert!(!bytes(&output).ends_with(b"\n"));
        assert_eq!(session.finish_error(input_error()), ExitCode::from(5));
        assert_eq!(bytes(&output), line[..line.len() - 1]);
    }

    #[test]
    fn terminal_write_failure_returns_io_and_never_appends_after_poison() {
        let started_line = serialize_line(&started()).unwrap();
        let (writer, output) =
            ScriptedWriter::new([WriteStep::All, WriteStep::Fail(ErrorKind::Other)]);
        let session = ProtocolSession::with_writer(writer, true, serialize_line);

        session.emit(started()).unwrap();
        assert_eq!(session.finish_result(result(), 1, 0), ExitCode::from(5));
        assert_eq!(bytes(&output), started_line);
        assert!(session.emit(started()).is_err());
        assert_eq!(bytes(&output), started_line);
    }

    #[test]
    fn serialization_failure_uses_fixed_terminal_without_the_general_serializer() {
        let (writer, output) = ScriptedWriter::new([WriteStep::All]);
        let session = ProtocolSession::with_writer(writer, true, failing_serializer);

        let error = session.emit(started()).unwrap_err();
        assert_eq!(
            error.reason(),
            ErrorReason::Internal(InternalReason::EventSerialization)
        );
        assert_eq!(bytes(&output), MINIMAL_SERIALIZATION_ERROR_LINE);
        assert_eq!(session.output_state(), OutputState::Terminal);
        assert_eq!(session.finish_error(error), ExitCode::from(6));
        assert_eq!(bytes(&output), MINIMAL_SERIALIZATION_ERROR_LINE);
    }

    #[test]
    fn serialization_exit_code_stays_internal_when_the_fixed_terminal_cannot_write() {
        let (writer, output) = ScriptedWriter::new([WriteStep::Fail(ErrorKind::Other)]);
        let session = ProtocolSession::with_writer(writer, true, failing_serializer);

        let error = session.emit(started()).unwrap_err();
        assert_eq!(session.output_state(), OutputState::Poisoned);
        assert_eq!(session.finish_error(error), ExitCode::from(6));
        assert!(bytes(&output).is_empty());
    }

    #[test]
    fn terminal_state_suppresses_all_later_events() {
        let (writer, output) = ScriptedWriter::new([WriteStep::All]);
        let session = ProtocolSession::with_writer(writer, true, serialize_line);

        assert_eq!(session.finish_result(result(), 1, 0), ExitCode::SUCCESS);
        let terminal = bytes(&output);
        session.emit(started()).unwrap();
        assert_eq!(bytes(&output), terminal);
        assert_eq!(session.output_state(), OutputState::Terminal);
    }
}
