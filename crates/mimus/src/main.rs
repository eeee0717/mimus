//! `mimus` - the CLI boundary defined by ADR-0001.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use mimus_core::engine::SingleLineLayoutDetector;
use mimus_core::engine::pdfium::PdfiumEngine;
use mimus_core::error::{IoReason, MimusError, UsageReason};
use mimus_core::event::{DiagnosticEvent, Event, EventKind, EventSink, ResultPayload, Stage};
use mimus_core::pass;
use mimus_core::translate::{NoneTranslator, openai_not_implemented};
use mimus_core::{Document, PassContext, PipelineConfig};

#[derive(Debug, Parser)]
#[command(
    name = "mimus",
    version = mimus_core::VERSION,
    about = "Layout-preserving PDF translation CLI"
)]
struct Cli {
    #[arg(long, global = true, help = "Emit versioned NDJSON events")]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Translate a native PDF while preserving its layout.
    Translate(TranslateArgs),
}

#[derive(Debug, clap::Args)]
struct TranslateArgs {
    /// Input native PDF.
    input: PathBuf,
    /// Output PDF path. Defaults to <stem>.zh.pdf.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Translation backend. M1 implements only the offline none backend.
    #[arg(long, value_enum, default_value_t = Backend::Openai)]
    backend: Backend,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    Openai,
    None,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(value) => value,
        Err(error) => {
            let success = matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            );
            let _ = error.print();
            return ExitCode::from(if success { 0 } else { 1 });
        }
    };
    let Some(command) = cli.command else {
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "{}", Cli::command().render_long_help());
        return ExitCode::SUCCESS;
    };
    let sink = CliEventSink::new(cli.json);
    match command {
        Command::Translate(args) => run_translate(args, &sink),
    }
}

fn run_translate(args: TranslateArgs, sink: &CliEventSink) -> ExitCode {
    let output = match args.output {
        Some(value) => value,
        None => match default_output_path(&args.input) {
            Ok(value) => value,
            Err(error) => return emit_error(sink, error),
        },
    };
    if matches!(args.backend, Backend::Openai) {
        return emit_error(sink, openai_not_implemented());
    }
    let engine = match PdfiumEngine::from_environment() {
        Ok(value) => value,
        Err(error) => return emit_error(sink, error),
    };
    let translator = NoneTranslator;
    let layout_detector = SingleLineLayoutDetector;
    let context = PassContext {
        engine: &engine,
        layout_detector: &layout_detector,
        translator: &translator,
        events: sink,
        snapshots: None,
        config: PipelineConfig::default(),
    };
    let mut document = Document::for_translation(args.input, output);
    match pass::run(&mut document, &context) {
        Ok(result) => {
            if let Err(error) = emit_diagnostics(sink, &document.diagnostics.events()) {
                return ExitCode::from(error.category().code());
            }
            match sink.emit(Event::new(EventKind::Result {
                result: ResultPayload::Translate {
                    output: result.output,
                },
                pages: result.pages,
                warnings: result.warnings,
            })) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => ExitCode::from(error.category().code()),
            }
        }
        Err(error) => {
            let _ = emit_diagnostics(sink, &document.diagnostics.events());
            emit_error(sink, error)
        }
    }
}

fn emit_diagnostics(
    sink: &CliEventSink,
    diagnostics: &[DiagnosticEvent],
) -> mimus_core::Result<()> {
    for diagnostic in diagnostics {
        sink.emit(Event::new(EventKind::Diagnostic {
            diagnostic: diagnostic.clone(),
        }))?;
    }
    Ok(())
}

fn default_output_path(input: &Path) -> Result<PathBuf, MimusError> {
    let stem = input
        .file_stem()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            MimusError::usage(
                UsageReason::InvalidArguments,
                format!("input path has no file stem: {}", input.display()),
            )
        })?;
    let mut filename = OsString::from(stem);
    filename.push(".zh.pdf");
    Ok(input.with_file_name(filename))
}

fn emit_error(sink: &CliEventSink, error: MimusError) -> ExitCode {
    let code = error.category().code();
    match sink.emit(Event::new(EventKind::from_error(&error))) {
        Ok(()) => ExitCode::from(code),
        Err(output_error) => ExitCode::from(output_error.category().code()),
    }
}

struct CliEventSink {
    json: bool,
    output_lock: Mutex<()>,
}

impl CliEventSink {
    const fn new(json: bool) -> Self {
        Self {
            json,
            output_lock: Mutex::new(()),
        }
    }
}

impl EventSink for CliEventSink {
    fn emit(&self, event: Event) -> mimus_core::Result<()> {
        let _guard = match self.output_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if self.json {
            let mut stdout = std::io::stdout().lock();
            let line = mimus_core::event::serialize_line(&event)?;
            return write_stdout(&mut stdout, &line);
        }
        match event.kind {
            EventKind::StageStarted { stage } => {
                let _ = writeln!(std::io::stderr().lock(), "{}...", stage_name(stage));
            }
            EventKind::PageProgress {
                stage,
                page_index,
                total_pages,
            } => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: page {}/{total_pages}",
                    stage_name(stage),
                    page_index + 1
                );
            }
            EventKind::StageFinished { .. } => {}
            EventKind::Diagnostic { diagnostic } => match diagnostic {
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
            },
            EventKind::Result { result, .. } => match result {
                ResultPayload::Translate { output } => {
                    write_stdout(
                        &mut std::io::stdout().lock(),
                        format!("{output}\n").as_bytes(),
                    )?;
                }
                ResultPayload::Inspect { il } => {
                    let bytes = mimus_core::il::canonical_json(&il)?;
                    write_stdout(&mut std::io::stdout().lock(), &bytes)?;
                }
            },
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
            }
        }
        Ok(())
    }
}

fn write_stdout(writer: &mut impl Write, bytes: &[u8]) -> mimus_core::Result<()> {
    match writer.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(MimusError::io(
            IoReason::StdoutWrite,
            format!("could not write stdout: {error}"),
        )),
    }
}

const fn stage_name(stage: Stage) -> &'static str {
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
    use super::*;

    #[test]
    fn default_output_uses_the_input_directory_and_stem() {
        assert_eq!(
            default_output_path(Path::new("reports/paper.pdf")).unwrap(),
            PathBuf::from("reports/paper.zh.pdf")
        );
    }
}
