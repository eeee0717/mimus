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
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["schema_version"] == 1));
    let terminal = events
        .iter()
        .filter(|event| matches!(event["event"].as_str(), Some("result" | "error")))
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["event"], "result");
    assert_eq!(terminal[0]["output"], translated.to_str().unwrap());
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
        .args(["translate", "--backend", "none", "--output"])
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
