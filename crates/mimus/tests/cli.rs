use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

const BIN: &str = env!("CARGO_BIN_EXE_mimus");
const PDFIUM_ENV: &str = "MIMUS_PDFIUM_LIBRARY";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture() -> PathBuf {
    fixture_path("unit-base-01-single-line")
}

fn fixture_path(id: &str) -> PathBuf {
    repo_root()
        .join("corpus/fixtures")
        .join(id)
        .join(format!("{id}.pdf"))
}

fn pdfium_library() -> OsString {
    let path = std::env::var_os(PDFIUM_ENV)
        .expect("MIMUS_PDFIUM_LIBRARY must point to the pinned test dylib");
    assert!(Path::new(&path).is_file(), "PDFium test library is missing");
    path
}

fn run_none(input: &Path, output: Option<&Path>, json: bool) -> Output {
    run_none_with_output_flag(input, output, json, "--output")
}

fn run_none_with_output_flag(
    input: &Path,
    output: Option<&Path>,
    json: bool,
    output_flag: &str,
) -> Output {
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    command.env("HTTP_PROXY", "http://127.0.0.1:9");
    command.env("HTTPS_PROXY", "http://127.0.0.1:9");
    command.env("OPENAI_API_KEY", "must-not-be-used");
    if json {
        command.arg("--json");
    }
    command.args(["translate", "--backend", "none"]);
    if let Some(output) = output {
        command.arg(output_flag).arg(output);
    }
    command.arg(input).output().unwrap()
}

fn run_inspect(input: &Path, json: bool, debug: Option<&Path>) -> Output {
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    if json {
        command.arg("--json");
    }
    command.arg("inspect");
    if let Some(debug) = debug {
        command.arg("--debug").arg(debug);
    }
    command.arg(input).output().unwrap()
}

fn run_none_with_debug(input: &Path, output: &Path, debug: &Path, json: bool) -> Output {
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    if json {
        command.arg("--json");
    }
    command
        .args(["translate", "--backend", "none", "--output"])
        .arg(output)
        .arg("--debug")
        .arg(debug)
        .arg(input)
        .output()
        .unwrap()
}

fn parse_events(output: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(output.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect()
}

fn assert_one_terminal_last(events: &[serde_json::Value], expected: &str) {
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["schema_version"] == 2));
    let terminal_indices = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event["event"].as_str(), Some("result" | "error")).then_some(index)
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_indices, vec![events.len() - 1]);
    assert_eq!(events.last().unwrap()["event"], expected);
}

fn directory_names(directory: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn write_program_pdf(directory: &Path, name: &str, program: &[u8]) -> PathBuf {
    let path = directory.join(name);
    let mut document = lopdf::Document::load(fixture()).unwrap();
    document
        .get_object_mut((9, 0))
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .set_plain_content(program.to_vec());
    document.save(&path).unwrap();
    path
}

#[test]
fn version_flag_reports_the_core_version() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains(mimus_core::VERSION)
    );
}

#[test]
fn help_and_bare_invocation_succeed() {
    let help = Command::new(BIN).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("translate"));
    assert!(help.contains("inspect"));
    assert!(help.contains("--json"));

    let bare = Command::new(BIN).output().unwrap();
    assert!(bare.status.success());
    assert!(!bare.stdout.is_empty());
}

#[test]
fn usage_errors_use_exit_code_one() {
    let output = Command::new(BIN).arg("not-a-command").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn json_usage_errors_are_one_terminal_event_and_metadata_stays_clap_text() {
    for arguments in [
        vec!["--json", "not-a-command"],
        vec!["--json", "inspect"],
        vec!["--json"],
    ] {
        let output = Command::new(BIN).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let events = parse_events(&output.stdout);
        assert_eq!(events.len(), 1);
        assert_one_terminal_last(&events, "error");
        assert_eq!(events[0]["schema_version"], 2);
        assert_eq!(events[0]["category"], "usage");
        assert_eq!(events[0]["reason"], "invalid_arguments");
        assert!(events[0]["message"].is_string());
        assert!(events[0]["hint"].is_null());
    }

    for flag in ["--help", "--version"] {
        let output = Command::new(BIN).args(["--json", flag]).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.starts_with(b"{"));
    }
}

#[test]
fn default_openai_backend_is_a_clear_unimplemented_error() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("paper.pdf");
    std::fs::copy(fixture(), &input).unwrap();
    let output = Command::new(BIN)
        .env_remove(PDFIUM_ENV)
        .args(["translate", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("backend_not_implemented"));
    assert!(stderr.contains("--backend none"));
    assert!(!directory.path().join("paper.zh.pdf").exists());
}

#[test]
fn none_backend_uses_the_default_output_path_without_network_access() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("paper.pdf");
    std::fs::copy(fixture(), &input).unwrap();
    let output = run_none(&input, None, false);
    assert!(
        output.status.success(),
        "none translation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let translated = directory.path().join("paper.zh.pdf");
    assert!(translated.is_file());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains(translated.to_str().unwrap())
    );
    let input_bytes = std::fs::read(input).unwrap();
    let output_bytes = std::fs::read(translated).unwrap();
    assert_eq!(&output_bytes[..input_bytes.len()], input_bytes);
}

#[test]
fn explicit_output_and_json_emit_one_versioned_terminal_event() {
    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("chosen.pdf");
    let output = run_none(&fixture(), Some(&translated), true);
    assert!(
        output.status.success(),
        "JSON translation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(translated.is_file());
    assert!(output.stderr.is_empty());
    let events = parse_events(&output.stdout);
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["schema_version"] == 2));
    assert_one_terminal_last(&events, "result");
    assert_eq!(
        events.last().unwrap()["output"],
        translated.to_str().unwrap()
    );
    assert!(events.last().unwrap().get("il").is_none());
    assert!(events.iter().any(|event| {
        event["event"] == "page_progress" && event["page_index"] == 0 && event.get("page").is_none()
    }));
}

#[test]
fn human_and_json_inspect_share_canonical_il_and_stop_at_the_read_only_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("paper.pdf");
    let debug = directory.path().join("inspect-debug");
    std::fs::copy(fixture(), &input).unwrap();

    let human = run_inspect(&input, false, Some(&debug));
    assert!(
        human.status.success(),
        "human inspect failed: {}",
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(!directory.path().join("paper.zh.pdf").exists());
    assert_eq!(
        human.stdout,
        std::fs::read(debug.join("03-paragraph_find.il.json")).unwrap()
    );
    insta::assert_snapshot!(
        "unit_base_01_human_inspect_il",
        String::from_utf8(human.stdout.clone()).unwrap()
    );
    let human_il: serde_json::Value = serde_json::from_slice(&human.stdout).unwrap();
    assert_eq!(human_il["schema_version"], 1);
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(stderr.contains("parse: page 1/1"));
    assert!(!stderr.contains("translate..."));
    assert!(!stderr.contains("write..."));

    let json = run_inspect(&input, true, None);
    assert!(json.status.success());
    assert!(json.stderr.is_empty());
    let events = parse_events(&json.stdout);
    assert!(events.iter().all(|event| event["schema_version"] == 2));
    assert_one_terminal_last(&events, "result");
    let started = events
        .iter()
        .filter(|event| event["event"] == "stage_started")
        .map(|event| event["stage"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        started,
        vec!["parse", "scan_detect", "layout", "paragraph_find"]
    );
    let terminal = events.last().unwrap();
    assert_eq!(terminal["il"], human_il);
    assert_eq!(terminal["il"]["schema_version"], 1);
    assert_eq!(terminal["pages"], 1);
    assert_eq!(terminal["warnings"], 0);
    assert!(terminal.get("output").is_none());
}

#[test]
fn debug_outputs_have_exact_stage_sets_and_no_temporary_files() {
    let directory = tempfile::tempdir().unwrap();
    let inspect_debug = directory.path().join("inspect-debug");
    let inspect = run_inspect(&fixture(), false, Some(&inspect_debug));
    assert!(inspect.status.success());
    assert_eq!(
        directory_names(&inspect_debug),
        vec![
            "00-parse.il.json",
            "01-scan_detect.il.json",
            "02-layout.il.json",
            "03-paragraph_find.il.json",
            "diagnostics.ndjson",
        ]
    );
    assert_eq!(
        std::fs::read(inspect_debug.join("diagnostics.ndjson")).unwrap(),
        b""
    );

    let translate_debug = directory.path().join("translate-debug");
    let translated = directory.path().join("translated.pdf");
    let translate = run_none_with_debug(&fixture(), &translated, &translate_debug, false);
    assert!(
        translate.status.success(),
        "debug translation failed: {}",
        String::from_utf8_lossy(&translate.stderr)
    );
    assert_eq!(
        directory_names(&translate_debug),
        vec![
            "00-parse.il.json",
            "01-scan_detect.il.json",
            "02-layout.il.json",
            "03-paragraph_find.il.json",
            "04-styles_and_formulas.il.json",
            "05-extract_terms.il.json",
            "06-translate.il.json",
            "07-typeset.il.json",
            "08-font_embed.il.json",
            "09-write.il.json",
            "diagnostics.ndjson",
        ]
    );
}

#[test]
fn failed_pass_keeps_debug_prefix_and_finishes_json_with_one_error() {
    let directory = tempfile::tempdir().unwrap();
    let input = fixture_path("unit-scan-01-image-only");
    let debug = directory.path().join("debug");

    let output = run_inspect(&input, true, Some(&debug));

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "error");
    assert_eq!(events.last().unwrap()["category"], "input");
    assert_eq!(events.last().unwrap()["reason"], "scanned_pdf");
    assert_eq!(
        directory_names(&debug),
        vec!["00-parse.il.json", "diagnostics.ndjson"]
    );
    let diagnostics = parse_events(&std::fs::read(debug.join("diagnostics.ndjson")).unwrap());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["id"], "scan_summary");
    assert_eq!(
        diagnostics[0]["scanned_page_indices"],
        serde_json::json!([0])
    );
}

#[test]
fn debug_directory_is_new_and_input_output_io_errors_are_typed() {
    let directory = tempfile::tempdir().unwrap();
    let debug = directory.path().join("debug");
    std::fs::create_dir(&debug).unwrap();
    std::fs::write(debug.join("sentinel"), b"keep").unwrap();
    let existing = run_inspect(&fixture(), true, Some(&debug));
    assert_eq!(existing.status.code(), Some(5));
    assert!(existing.stderr.is_empty());
    let events = parse_events(&existing.stdout);
    assert_one_terminal_last(&events, "error");
    assert_eq!(events[0]["category"], "io");
    assert_eq!(events[0]["reason"], "debug_write");
    assert_eq!(directory_names(&debug), vec!["sentinel"]);

    let missing = directory.path().join("missing.pdf");
    let input_error = run_inspect(&missing, true, None);
    assert_eq!(input_error.status.code(), Some(5));
    assert!(input_error.stderr.is_empty());
    let events = parse_events(&input_error.stdout);
    assert_one_terminal_last(&events, "error");
    assert_eq!(events.last().unwrap()["category"], "io");
    assert_eq!(events.last().unwrap()["reason"], "input_read");

    let output = directory.path().join("missing-parent/output.pdf");
    let output_error = run_none(&fixture(), Some(&output), true);
    assert_eq!(output_error.status.code(), Some(5));
    assert!(output_error.stderr.is_empty());
    let events = parse_events(&output_error.stdout);
    assert_one_terminal_last(&events, "error");
    assert_eq!(events.last().unwrap()["category"], "io");
    assert_eq!(events.last().unwrap()["reason"], "output_write");
}

#[test]
fn short_output_flag_is_supported() {
    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("short.pdf");
    let output = run_none_with_output_flag(&fixture(), Some(&translated), false, "-o");
    assert!(
        output.status.success(),
        "short output flag failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(translated.is_file());
}

#[test]
fn unsupported_existing_fixtures_fail_closed_without_output() {
    let directory = tempfile::tempdir().unwrap();
    for id in [
        "unit-base-02-two-column",
        "unit-base-03-structured",
        "unit-stream-08-inline-image-EI-in-data",
        "unit-geom-01-rotate-90",
    ] {
        let translated = directory.path().join(format!("{id}.pdf"));
        let result = run_none(&fixture_path(id), Some(&translated), false);
        assert_eq!(
            result.status.code(),
            Some(2),
            "fixture {id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("unsupported_pdf"),
            "fixture {id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!translated.exists(), "fixture {id} produced output");
    }
}

#[test]
fn unreplayed_state_graphics_and_multiple_lines_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let programs: &[(&str, &[u8])] = &[
        (
            "scaled-ctm.pdf",
            b"0.5 0 0 0.5 0 0 cm\nBT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
        ),
        (
            "character-spacing.pdf",
            b"BT /F1 12 Tf 2 Tc 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
        ),
        (
            "invisible-text.pdf",
            b"BT /F1 12 Tf 3 Tr 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
        ),
        (
            "two-lines.pdf",
            b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj 1 0 0 1 72 80 Tm (MIMUS) Tj ET",
        ),
        (
            "rectangle.pdf",
            b"0 0 10 10 re f BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
        ),
    ];
    for (name, program) in programs {
        let input = write_program_pdf(directory.path(), name, program);
        let translated = directory.path().join(format!("out-{name}"));
        let result = run_none(&input, Some(&translated), false);
        assert_eq!(
            result.status.code(),
            Some(2),
            "input {name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!translated.exists(), "input {name} produced output");
    }
}

#[test]
fn missing_pdfium_uses_asset_exit_code_three() {
    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("missing-pdfium.pdf");
    let result = Command::new(BIN)
        .env(PDFIUM_ENV, directory.path().join("missing-libpdfium.dylib"))
        .args(["translate", "--backend", "none", "--output"])
        .arg(&translated)
        .arg(fixture())
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&result.stderr).contains("pdfium_unavailable"));
    assert!(!translated.exists());
}

#[test]
fn a_closed_stdout_pipe_does_not_turn_success_into_a_panic() {
    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("broken-pipe.pdf");
    let mut child = Command::new(BIN)
        .env(PDFIUM_ENV, pdfium_library())
        .args(["--json", "translate", "--backend", "none", "--output"])
        .arg(&translated)
        .arg(fixture())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(translated.exists());
}

#[test]
fn independent_tools_open_the_output_and_preserve_manifest_geometry() {
    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("validated.pdf");
    let output = run_none(&fixture(), Some(&translated), false);
    assert!(output.status.success());

    let qpdf = Command::new("qpdf")
        .arg("--check")
        .arg(&translated)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "{}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    let input_pages = qpdf_pages(&fixture());
    let output_pages = qpdf_pages(&translated);
    assert_eq!(output_pages[0]["object"], input_pages[0]["object"]);
    let input_content = input_pages[0]["contents"][0].as_str().unwrap();
    let output_content = output_pages[0]["contents"][0].as_str().unwrap();
    assert_ne!(output_content, input_content);
    let input_object = input_content
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let output_object = output_content
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert!(output_object > input_object);
    let active_stream = Command::new("qpdf")
        .arg(format!("--show-object={output_object}"))
        .arg("--raw-stream-data")
        .arg(&translated)
        .output()
        .unwrap();
    assert!(active_stream.status.success());
    assert_eq!(
        active_stream.stdout,
        b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n"
    );

    let poppler = Command::new("pdftotext")
        .arg("-bbox-layout")
        .arg(&translated)
        .arg("-")
        .output()
        .unwrap();
    assert!(poppler.status.success());
    let word = first_element_attributes(&poppler.stdout, b"word");
    assert_close(number(&word, "xMin"), 72.0, 0.001);
    assert_close(number(&word, "xMax"), 112.656, 0.001);
    assert_close(200.0 - number(&word, "yMax"), 117.168, 0.001);
    assert_close(200.0 - number(&word, "yMin"), 131.136, 0.001);

    let mupdf = Command::new("mutool")
        .args(["draw", "-F", "stext", "-o", "-"])
        .arg(&translated)
        .arg("1")
        .output()
        .unwrap();
    assert!(mupdf.status.success());
    let character = first_element_attributes(&mupdf.stdout, b"char");
    assert_eq!(character.get("c").map(String::as_str), Some("M"));
    assert_close(number(&character, "x"), 72.0, 0.001);
    assert_close(200.0 - number(&character, "y"), 120.0, 0.001);
}

fn first_element_attributes(xml: &[u8], name: &[u8]) -> BTreeMap<String, String> {
    let mut reader = Reader::from_reader(xml);
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == name =>
            {
                return element
                    .attributes()
                    .map(|attribute| {
                        let attribute = attribute.unwrap();
                        (
                            String::from_utf8(attribute.key.local_name().as_ref().to_vec())
                                .unwrap(),
                            attribute
                                .normalized_value(XmlVersion::Implicit1_0)
                                .unwrap()
                                .into_owned(),
                        )
                    })
                    .collect();
            }
            Event::Eof => panic!("element {} not found", String::from_utf8_lossy(name)),
            _ => {}
        }
    }
}

fn qpdf_pages(pdf: &Path) -> Vec<serde_json::Value> {
    let output = Command::new("qpdf")
        .args(["--json", "--json-key=pages"])
        .arg(pdf)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["pages"]
        .as_array()
        .unwrap()
        .clone()
}

fn number(attributes: &BTreeMap<String, String>, name: &str) -> f64 {
    attributes[name].parse().unwrap()
}

fn assert_close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected} +/- {tolerance}, got {actual}"
    );
}
