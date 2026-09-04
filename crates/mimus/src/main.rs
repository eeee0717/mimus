//! `mimus` - the CLI boundary defined by ADR-0001.

mod config;
mod debug;
mod font_assets;
mod layout_assets;
mod protocol;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use config::{Backend, ConfigOverrides, ResolvedConfig, ResolvedLayoutConfig};
use debug::DebugArtifacts;
use mimus_core::engine::pdfium::PdfiumEngine;
use mimus_core::engine::{
    LayoutDetector, OnnxLayoutDetector, RecordedLayoutDetector, SingleLineLayoutDetector,
};
use mimus_core::error::{ErrorReason, InternalReason, IoReason, MimusError, Result, UsageReason};
use mimus_core::event::{ConfigurationResolved, Event, EventKind, EventSink, ResultPayload};
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
    /// Regular Latin output font file.
    #[arg(long, value_name = "TTF_OR_OTF", conflicts_with = "font_fallback")]
    font_latin: Option<PathBuf>,
    /// Bold Latin output font file.
    #[arg(long, value_name = "TTF_OR_OTF", conflicts_with = "font_fallback_bold")]
    font_latin_bold: Option<PathBuf>,
    /// Deprecated alias for --font-latin.
    #[arg(long, value_name = "TTF_OR_OTF")]
    font_fallback: Option<PathBuf>,
    /// Deprecated alias for --font-latin-bold.
    #[arg(long, value_name = "TTF_OR_OTF")]
    font_fallback_bold: Option<PathBuf>,
    /// PP-DocLayoutV3 ONNX model file.
    #[arg(long, value_name = "ONNX")]
    layout_model: Option<PathBuf>,
    /// Layout detector implementation.
    #[arg(long, value_enum, default_value_t = LayoutMode::Onnx)]
    layout: LayoutMode,
    /// Base URL used to mirror model and output-font assets.
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
    /// Per-request provider timeout in seconds (1-600).
    #[arg(long, value_name = "SECONDS")]
    request_timeout: Option<i64>,
    /// Fail without publishing output when any page or paragraph is preserved.
    #[arg(long)]
    strict: bool,
    /// Experimental: translate text within recognized table-cell boundaries.
    #[arg(long)]
    translate_table: bool,
    /// Remove visible borders from Link annotations while preserving their targets and rectangles.
    #[arg(long)]
    strip_link_borders: bool,
    /// Publish each original page followed by its translated counterpart.
    #[arg(long)]
    bilingual: bool,
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
    /// PP-DocLayoutV3 ONNX model file.
    #[arg(long, value_name = "ONNX")]
    layout_model: Option<PathBuf>,
    /// Layout detector implementation.
    #[arg(long, value_enum, default_value_t = LayoutMode::Onnx)]
    layout: LayoutMode,
    /// Base URL used to mirror layout-model assets.
    #[arg(long, value_name = "URL")]
    asset_mirror: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum LayoutMode {
    Onnx,
    SingleLine,
}

impl LayoutMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Onnx => "onnx",
            Self::SingleLine => "single_line",
        }
    }
}

struct CreatedLayoutDetector {
    detector: Box<dyn LayoutDetector>,
    mode: &'static str,
    model_source: Option<String>,
    model_sha256: Option<String>,
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
        font_latin: args.font_latin.or(args.font_fallback),
        font_latin_bold: args.font_latin_bold.or(args.font_fallback_bold),
        layout_model: args.layout_model,
        asset_mirror: args.asset_mirror,
        glossary: args.glossary,
        dump_glossary: args.dump_glossary,
        no_auto_terms: args.no_auto_terms,
        cache: args.cache,
        no_cache: args.no_cache,
        concurrency: args.concurrency,
        request_timeout_secs: args.request_timeout,
        strict: args.strict,
        translate_table: args.translate_table,
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
        font_assets::FontSelections {
            regular: resolved.font_regular.as_ref(),
            bold: resolved.font_bold.as_ref(),
            latin_regular: resolved.font_latin.as_ref(),
            latin_bold: resolved.font_latin_bold.as_ref(),
        },
        font_assets::FontCacheDirs {
            cjk: &resolved.font_cjk_cache_dir,
            latin: &resolved.font_latin_cache_dir,
            latin_symbol: &resolved.font_latin_symbol_cache_dir,
        },
        resolved.asset_mirror.as_deref(),
    ) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let font_regular_source = output_fonts.regular.source.clone();
    let font_regular_sha256 = output_fonts.regular.sha256.clone();
    let font_bold_source = output_fonts.bold.source.clone();
    let font_bold_sha256 = output_fonts.bold.sha256.clone();
    let font_latin_source = output_fonts.latin_regular.source.clone();
    let font_latin_sha256 = output_fonts.latin_regular.sha256.clone();
    let font_latin_bold_source = output_fonts.latin_bold.source.clone();
    let font_latin_bold_sha256 = output_fonts.latin_bold.sha256.clone();
    let font_latin_symbol_source = output_fonts.latin_symbol.source.clone();
    let font_latin_symbol_sha256 = output_fonts.latin_symbol.sha256.clone();
    let layout_detector = match create_layout_detector(
        args.layout,
        args.layout_replay.as_deref(),
        resolved.layout_model.as_ref(),
        &resolved.layout_model_cache_dir,
        resolved.asset_mirror.as_deref(),
    ) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let glossary_fingerprint = resolved.user_glossary.fingerprint();
    let cache_path = resolved.cache_path.clone();
    let cache_enabled = cache_path.is_some();
    let cache_path_display = cache_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    let max_concurrency = resolved.max_concurrency;
    let request_timeout_secs = resolved.request_timeout_secs;
    let strict = resolved.strict;
    let translate_table = resolved.translate_table;
    let strip_link_borders = args.strip_link_borders;
    let bilingual = args.bilingual;
    let translator = match resolved.take_translator() {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    if let Err(error) = session.emit(Event::new(EventKind::ConfigurationResolved {
        configuration: Box::new(ConfigurationResolved {
            backend,
            endpoint: Some(endpoint),
            model: Some(model),
            target_language: target_language.clone(),
            font_regular_source: Some(font_regular_source),
            font_regular_sha256: Some(font_regular_sha256),
            font_bold_source: Some(font_bold_source),
            font_bold_sha256: Some(font_bold_sha256),
            font_latin_source: Some(font_latin_source.clone()),
            font_latin_sha256: Some(font_latin_sha256.clone()),
            font_latin_bold_source: Some(font_latin_bold_source.clone()),
            font_latin_bold_sha256: Some(font_latin_bold_sha256.clone()),
            font_latin_symbol_source: Some(font_latin_symbol_source),
            font_latin_symbol_sha256: Some(font_latin_symbol_sha256),
            font_fallback_regular_source: Some(font_latin_source),
            font_fallback_regular_sha256: Some(font_latin_sha256),
            font_fallback_bold_source: Some(font_latin_bold_source),
            font_fallback_bold_sha256: Some(font_latin_bold_sha256),
            layout_mode: layout_detector.mode.to_owned(),
            layout_model_source: layout_detector.model_source.clone(),
            layout_model_sha256: layout_detector.model_sha256.clone(),
            auto_terms,
            glossary_fingerprint,
            cache_enabled,
            cache_path: cache_path_display,
            concurrency: max_concurrency,
            request_timeout_secs,
            strict,
            translate_table,
            strip_link_borders,
            bilingual,
        }),
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
    let context = PassContext {
        engine: &engine,
        layout_detector: layout_detector.detector.as_ref(),
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
            translate_table,
            strip_link_borders,
            bilingual,
            ..PipelineConfig::default()
        },
    };
    let mut document = Document::for_translation(args.input, output);
    let outcome = pass::run(&mut document, &context).map(|result| CommandOutcome {
        result: ResultPayload::Translate {
            output: result.output,
            translate_table,
            strip_link_borders,
            bilingual,
        },
        pages: result.pages,
        warnings: result.warnings,
    });
    finish_command(session, debug.as_ref(), &document, outcome)
}

fn run_inspect(args: InspectArgs, session: &ProtocolSession) -> ExitCode {
    let layout_config = if args.layout_replay.is_some() || args.layout == LayoutMode::SingleLine {
        None
    } else {
        match ResolvedLayoutConfig::load(args.layout_model, args.asset_mirror) {
            Ok(value) => Some(value),
            Err(error) => return session.finish_error(error),
        }
    };
    let layout_detector = match create_layout_detector(
        args.layout,
        args.layout_replay.as_deref(),
        layout_config
            .as_ref()
            .and_then(|config| config.layout_model.as_ref()),
        layout_config
            .as_ref()
            .map_or_else(|| Path::new(""), |config| &config.layout_model_cache_dir),
        layout_config
            .as_ref()
            .and_then(|config| config.asset_mirror.as_deref()),
    ) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let engine = match PdfiumEngine::from_environment() {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let debug = match create_debug(args.debug) {
        Ok(value) => value,
        Err(error) => return session.finish_error(error),
    };
    let translator = NoneTranslator;
    let context = PassContext {
        engine: &engine,
        layout_detector: layout_detector.detector.as_ref(),
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
    if let Err(error) = &outcome
        && is_protocol_failure(error)
    {
        return session.finish_error(outcome.unwrap_err());
    }

    if let Some(debug) = debug
        && let Err(error) = debug.write_diagnostics(&document.diagnostics.debug_events())
    {
        return session.finish_error(error);
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

fn create_layout_detector(
    mode: LayoutMode,
    replay: Option<&Path>,
    explicit_model: Option<&config::LayoutModelPathSelection>,
    model_cache_dir: &Path,
    asset_mirror: Option<&str>,
) -> Result<CreatedLayoutDetector> {
    if let Some(path) = replay {
        let bytes = std::fs::read(path).map_err(|error| {
            MimusError::io(
                IoReason::InputRead,
                format!(
                    "could not read layout recording {}: {error}",
                    path.display()
                ),
            )
        })?;
        return Ok(CreatedLayoutDetector {
            detector: Box::new(RecordedLayoutDetector::from_bytes(&bytes)?),
            mode: "replay",
            model_source: None,
            model_sha256: None,
        });
    }
    if mode == LayoutMode::SingleLine {
        return Ok(CreatedLayoutDetector {
            detector: Box::new(SingleLineLayoutDetector),
            mode: mode.as_str(),
            model_source: None,
            model_sha256: None,
        });
    }
    let model = layout_assets::resolve_layout_model(explicit_model, model_cache_dir, asset_mirror)?;
    Ok(CreatedLayoutDetector {
        detector: Box::new(OnnxLayoutDetector::from_file(&model.path)?),
        mode: mode.as_str(),
        model_source: Some(model.source),
        model_sha256: Some(model.sha256),
    })
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
