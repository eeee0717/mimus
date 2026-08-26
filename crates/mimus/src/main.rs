//! `mimus` - the CLI boundary defined by ADR-0001.

mod config;
mod debug;
mod font_assets;
mod protocol;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use config::{Backend, ConfigOverrides, ResolvedConfig};
use debug::DebugArtifacts;
use mimus_core::engine::pdfium::PdfiumEngine;
use mimus_core::engine::{LayoutDetector, RecordedLayoutDetector, SingleLineLayoutDetector};
use mimus_core::error::{ErrorReason, InternalReason, IoReason, MimusError, Result, UsageReason};
use mimus_core::event::{Event, EventKind, EventSink, ResultPayload};
use mimus_core::pass;
use mimus_core::translate::NoneTranslator;
use mimus_core::{Document, PassContext, PassSnapshotSink, PipelineConfig};
use protocol::ProtocolSession;

#[derive(Debug, Parser)]
#[command(
    name = "mimus",
    version = mimus_core::VERSION,
    about = "Layout-preserving PDF translation CLI",
    color = clap::ColorChoice::Never
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
    Translate(Box<TranslateArgs>),
    /// Inspect the IL produced by the read-only pipeline prefix.
    Inspect(InspectArgs),
}

#[derive(Debug, clap::Args)]
struct TranslateArgs {
    /// Input native PDF.
    input: PathBuf,
    /// Output PDF path. Defaults to <stem>.zh.pdf.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Translation backend.
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// OpenAI-compatible API base URL.
    #[arg(long)]
    endpoint: Option<String>,
    /// OpenAI-compatible model ID.
    #[arg(long)]
    model: Option<String>,
    /// Translation target language.
    #[arg(long)]
    target_language: Option<String>,
    /// Regular output font file.
    #[arg(long, value_name = "TTF_OR_OTF")]
    font: Option<PathBuf>,
    /// Bold output font file.
    #[arg(long, value_name = "TTF_OR_OTF")]
    font_bold: Option<PathBuf>,
    /// Base URL used to mirror output-font assets.
    #[arg(long, value_name = "URL")]
    asset_mirror: Option<String>,
    /// User glossary TOML file. User entries override automatically extracted terms.
    #[arg(long, value_name = "TOML")]
    glossary: Option<PathBuf>,
    /// Write the final canonical glossary to this path.
    #[arg(long, value_name = "TOML")]
    dump_glossary: Option<PathBuf>,
    /// Skip automatic document-level term extraction.
    #[arg(long)]
    no_auto_terms: bool,
    /// Translation cache database path.
    #[arg(long, value_name = "REDB", conflicts_with = "no_cache")]
    cache: Option<PathBuf>,
    /// Bypass translation cache reads and writes.
    #[arg(long)]
    no_cache: bool,
    /// Maximum number of paragraph translation requests in flight.
    #[arg(long, value_name = "COUNT")]
    concurrency: Option<usize>,
    /// Fail without publishing output when any page or paragraph is preserved.
    #[arg(long)]
    strict: bool,
    /// New directory for per-pass IL snapshots and diagnostics.
    #[arg(long, value_name = "NEW_DIR")]
    debug: Option<PathBuf>,
    /// Deterministic detector recording used by Corpus and explicit local validation.
    #[arg(long, value_name = "JSON", hide = true)]
    layout_replay: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct InspectArgs {
    /// Input native PDF.
    input: PathBuf,
    /// New directory for per-pass IL snapshots and diagnostics.
    #[arg(long, value_name = "NEW_DIR")]
    debug: Option<PathBuf>,
    /// Deterministic detector recording used by Corpus and explicit local validation.
    #[arg(long, value_name = "JSON", hide = true)]
    layout_replay: Option<PathBuf>,
}

#[derive(Debug)]
struct CommandOutcome {
    result: ResultPayload,
    pages: usize,
    warnings: usize,
}

fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let json_requested = requests_json(&arguments);
    let cli = match Cli::try_parse_from(&arguments) {
        Ok(value) => value,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            if json_requested {
                return ProtocolSession::stdout(true).finish_error(MimusError::usage(
                    UsageReason::InvalidArguments,
                    error.to_string(),
                ));
            }
            let _ = error.print();
            return ExitCode::from(1);
        }
    };

    let Some(command) = cli.command else {
        if cli.json {
            return ProtocolSession::stdout(true).finish_error(MimusError::usage(
                UsageReason::InvalidArguments,
                "a subcommand is required",
            ));
        }
        return write_bare_help();
    };

    let session = ProtocolSession::stdout(cli.json);
    match command {
        Command::Translate(args) => run_translate(*args, &session),
        Command::Inspect(args) => run_inspect(args, &session),
    }
}

fn run_translate(args: TranslateArgs, session: &ProtocolSession) -> ExitCode {
    let output = match args.output {
        Some(value) => value,
        None => match default_output_path(&args.input) {
            Ok(value) => value,
            Err(error) => return session.finish_error(error),
        },
    };
    let mut resolved = match ResolvedConfig::load(ConfigOverrides {
        backend: args.backend,
        base_url: args.endpoint,
        model: args.model,
        target_language: args.target_language,
        font_regular: args.font,
        font_bold: args.font_bold,
        asset_mirror: args.asset_mirror,
        glossary: args.glossary,
        dump_glossary: args.dump_glossary,
        no_auto_terms: args.no_auto_terms,
        cache: args.cache,
        no_cache: args.no_cache,
        concurrency: args.concurrency,
        strict: args.strict,
    }) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let target_language = resolved.target_language.clone();
    let user_glossary = resolved.user_glossary.clone();
    let auto_terms = resolved.auto_terms;
    let dump_glossary = resolved.dump_glossary.clone();
    let backend = resolved.backend.as_str().to_owned();
    let endpoint = resolved.base_url.clone();
    let model = resolved.model.clone();
    let output_fonts = match font_assets::resolve_fonts(
        resolved.font_regular.as_ref(),
        resolved.font_bold.as_ref(),
        &resolved.font_cache_dir,
        resolved.asset_mirror.as_deref(),
    ) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let font_regular_source = output_fonts.regular.source.clone();
    let font_regular_sha256 = output_fonts.regular.sha256.clone();
    let font_bold_source = output_fonts.bold.source.clone();
    let font_bold_sha256 = output_fonts.bold.sha256.clone();
    let glossary_fingerprint = resolved.user_glossary.fingerprint();
    let cache_path = resolved.cache_path.clone();
    let cache_enabled = cache_path.is_some();
    let cache_path_display = cache_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    let max_concurrency = resolved.max_concurrency;
    let strict = resolved.strict;
    let translator = match resolved.take_translator() {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    if let Err(error) = session.emit(Event::new(EventKind::ConfigurationResolved {
        backend,
        endpoint: Some(endpoint),
        model: Some(model),
        target_language: target_language.clone(),
        font_regular_source: Some(font_regular_source),
        font_regular_sha256: Some(font_regular_sha256),
        font_bold_source: Some(font_bold_source),
        font_bold_sha256: Some(font_bold_sha256),
        auto_terms,
        glossary_fingerprint,
        cache_enabled,
        cache_path: cache_path_display,
        concurrency: max_concurrency,
        strict,
    })) {
        return session.finish_error(error);
    }
    let engine = match PdfiumEngine::from_environment() {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let debug = match create_debug(args.debug) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let layout_detector = match create_layout_detector(args.layout_replay.as_deref()) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let context = PassContext {
        engine: &engine,
        layout_detector: layout_detector.as_ref(),
        translator: translator.as_ref(),
        events: session,
        snapshots: debug.as_ref().map(|value| value as &dyn PassSnapshotSink),
        config: PipelineConfig {
            target_language,
            output_fonts: Some(output_fonts),
            user_glossary,
            auto_terms,
            dump_glossary,
            cache_path,
            max_concurrency,
            strict,
            ..PipelineConfig::default()
        },
    };
    let mut document = Document::for_translation(args.input, output);
    let outcome = pass::run(&mut document, &context).map(|result| CommandOutcome {
        result: ResultPayload::Translate {
            output: result.output,
        },
        pages: result.pages,
        warnings: result.warnings,
    });
    finish_command(session, debug.as_ref(), &document, outcome)
}

fn run_inspect(args: InspectArgs, session: &ProtocolSession) -> ExitCode {
    let engine = match PdfiumEngine::from_environment() {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let debug = match create_debug(args.debug) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let translator = NoneTranslator;
    let layout_detector = match create_layout_detector(args.layout_replay.as_deref()) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let context = PassContext {
        engine: &engine,
        layout_detector: layout_detector.as_ref(),
        translator: &translator,
        events: session,
        snapshots: debug.as_ref().map(|value| value as &dyn PassSnapshotSink),
        config: PipelineConfig::default(),
    };
    let mut document = Document::for_inspection(args.input);
    let outcome = pass::inspect(&mut document, &context).map(|result| CommandOutcome {
        result: ResultPayload::Inspect { il: result.il },
        pages: result.pages,
        warnings: result.warnings,
    });
    finish_command(session, debug.as_ref(), &document, outcome)
}

fn finish_command(
    session: &ProtocolSession,
    debug: Option<&DebugArtifacts>,
    document: &Document,
    outcome: Result<CommandOutcome>,
) -> ExitCode {
    if let Err(error) = &outcome {
        if is_protocol_failure(error) {
            return session.finish_error(outcome.unwrap_err());
        }
    }

    if let Some(debug) = debug {
        if let Err(error) = debug.write_diagnostics(&document.diagnostics.debug_events()) {
            return session.finish_error(error);
        }
    }
    let diagnostics = document.diagnostics.events();
    if let Err(error) = session.emit_diagnostics(&diagnostics) {
        return session.finish_error(error);
    }

    match outcome {
        Ok(outcome) => session.finish_result(outcome.result, outcome.pages, outcome.warnings),
        Err(error) => session.finish_error(error),
    }
}

fn create_debug(path: Option<PathBuf>) -> Result<Option<DebugArtifacts>> {
    path.map(DebugArtifacts::create).transpose()
}

fn create_layout_detector(path: Option<&Path>) -> Result<Box<dyn LayoutDetector>> {
    let Some(path) = path else {
        return Ok(Box::new(SingleLineLayoutDetector));
    };
    let bytes = std::fs::read(path).map_err(|error| {
        MimusError::io(
            IoReason::InputRead,
            format!(
                "could not read layout recording {}: {error}",
                path.display()
            ),
        )
    })?;
    Ok(Box::new(RecordedLayoutDetector::from_bytes(&bytes)?))
}

fn is_protocol_failure(error: &MimusError) -> bool {
    matches!(
        error.reason(),
        ErrorReason::Io(IoReason::StdoutWrite)
            | ErrorReason::Internal(InternalReason::EventSerialization)
    )
}

fn requests_json(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .skip(1)
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| argument.as_os_str() == OsStr::new("--json"))
}

fn write_bare_help() -> ExitCode {
    let help = Cli::command().render_long_help().to_string();
    match std::io::stdout().lock().write_all(help.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            let error = MimusError::io(
                IoReason::StdoutWrite,
                format!("could not write stdout: {error}"),
            );
            let _ = writeln!(
                std::io::stderr().lock(),
                "error[{}]: {}",
                error.reason(),
                error
            );
            ExitCode::from(error.category().code())
        }
    }
}

fn default_output_path(input: &Path) -> Result<PathBuf> {
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

    #[test]
    fn json_flag_detection_stops_at_the_argument_separator() {
        assert!(requests_json(&[
            OsString::from("mimus"),
            OsString::from("inspect"),
            OsString::from("--json"),
        ]));
        assert!(!requests_json(&[
            OsString::from("mimus"),
            OsString::from("inspect"),
            OsString::from("--"),
            OsString::from("--json"),
        ]));
    }

    #[test]
    fn production_event_serializer_is_the_core_serializer() {
        let _serializer: fn(&mimus_core::event::Event) -> Result<Vec<u8>> =
            mimus_core::event::serialize_line;
    }
}
