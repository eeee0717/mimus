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
use mimus_core::error::{MimusError, UsageReason};
use mimus_core::event::{Event, EventKind, EventSink, Stage};
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
        config: PipelineConfig::default(),
    };
    let mut document = Document::new(args.input, output);
    match pass::run(&mut document, &context) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => ExitCode::from(error.category().code()),
    }
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
    sink.emit(Event::new(EventKind::from_error(&error)));
    ExitCode::from(code)
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
    fn emit(&self, event: Event) {
        let _guard = match self.output_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if self.json {
            let mut stdout = std::io::stdout().lock();
            if serde_json::to_writer(&mut stdout, &event).is_ok() {
                let _ = stdout.write_all(b"\n");
            }
            return;
        }
        match event.kind {
            EventKind::StageStarted { stage } => {
                let _ = writeln!(std::io::stderr().lock(), "{}...", stage_name(stage));
            }
            EventKind::PageProgress {
                stage,
                page,
                total_pages,
            } => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: page {page}/{total_pages}",
                    stage_name(stage)
                );
            }
            EventKind::StageFinished { .. } => {}
            EventKind::Result { output, .. } => {
                let _ = writeln!(std::io::stdout().lock(), "{output}");
            }
            EventKind::Error {
                reason,
                message,
                hint,
            } => {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "error[{reason}]: {message}");
                if let Some(hint) = hint {
                    let _ = writeln!(stderr, "hint: {hint}");
                }
            }
        }
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
