use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use serde::Deserialize;

const BIN: &str = env!("CARGO_BIN_EXE_mimus");
const PDFIUM_ENV: &str = "MIMUS_PDFIUM_LIBRARY";

struct FakeResponsesServer {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeResponsesServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let requests = Arc::clone(&captured);
                        handlers.push(thread::spawn(move || {
                            handle_responses_request(&mut stream, &requests);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("fake Responses server accept failed: {error}"),
                }
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        Self {
            endpoint: format!("http://{address}"),
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeResponsesServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.endpoint.trim_start_matches("http://"));
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn handle_responses_request(stream: &mut TcpStream, requests: &Arc<Mutex<Vec<String>>>) {
    let request = read_http_request(stream);
    if request.is_empty() {
        return;
    }
    let header_end = request
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap()
        + 4;
    let payload: serde_json::Value = serde_json::from_slice(&request[header_end..]).unwrap();
    let input = payload["input"].as_str().unwrap().to_owned();
    requests.lock().unwrap().push(input.clone());
    let (status, body) = if input == "first" {
        (
            "400 Bad Request",
            r#"{"error":"injected table-cell failure"}"#,
        )
    } else {
        ("200 OK", r#"{"output_text":"M"}"#)
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn read_http_request(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap_or(0);
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + content_length {
            break;
        }
    }
    request
}

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

fn manifest_path(id: &str) -> PathBuf {
    repo_root()
        .join("corpus/fixtures")
        .join(id)
        .join("manifest.toml")
}

fn layout_recording_path(id: &str) -> PathBuf {
    repo_root()
        .join("corpus/layout-recordings")
        .join(format!("{id}.json"))
}

fn test_font_path(weight: &str) -> PathBuf {
    repo_root()
        .join("crates/mimus/tests/assets/fonts")
        .join(format!("MimusTestGB2312-{weight}.ttf"))
}

fn test_fallback_font_path(weight: &str) -> PathBuf {
    repo_root()
        .join("crates/mimus/tests/assets/fonts")
        .join(format!("MimusTestFallback-{weight}.ttf"))
}

fn configure_test_fonts(command: &mut Command) {
    command.env("MIMUS_FONT_REGULAR", test_font_path("Regular"));
    command.env("MIMUS_FONT_BOLD", test_font_path("Bold"));
    command.env(
        "MIMUS_FONT_FALLBACK_REGULAR",
        test_fallback_font_path("Regular"),
    );
    command.env("MIMUS_FONT_FALLBACK_BOLD", test_fallback_font_path("Bold"));
}

#[derive(Debug, Deserialize)]
struct FixtureManifest {
    identity: ManifestIdentity,
    page: Vec<ManifestPage>,
    expected: ManifestExpected,
}

#[derive(Debug, Deserialize)]
struct ManifestIdentity {
    cases: Vec<String>,
    legality: String,
}

#[derive(Debug, Deserialize)]
struct ManifestPage {
    media_box: [ManifestCoordinate; 4],
    #[serde(default)]
    crop_box: Option<[f64; 4]>,
    rotate: i32,
}

impl ManifestPage {
    fn effective_box(&self) -> Option<[f64; 4]> {
        self.crop_box.or_else(|| {
            let mut result = [0.0; 4];
            for (index, coordinate) in self.media_box.iter().enumerate() {
                result[index] = match coordinate {
                    ManifestCoordinate::Number(value) => *value,
                    ManifestCoordinate::Keyword(keyword) => {
                        assert_eq!(keyword, "null");
                        return None;
                    }
                };
            }
            Some(result)
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ManifestCoordinate {
    Number(f64),
    Keyword(String),
}

#[derive(Debug, Default, Deserialize)]
struct ManifestExpected {
    #[serde(default)]
    block: Vec<ManifestBlock>,
    #[serde(default)]
    transform: Vec<ManifestTransform>,
    #[serde(default)]
    degradation: Vec<ManifestDegradation>,
    #[serde(default)]
    alignment: Vec<ManifestAlignment>,
}

#[derive(Debug, Deserialize)]
struct ManifestBlock {
    key: String,
    page: usize,
    draw_order: usize,
    reading_order: usize,
    text: String,
}

#[derive(Debug, Deserialize)]
struct ManifestTransform {
    block: String,
    char_indices: Vec<usize>,
    kind: String,
    #[serde(default)]
    degrees: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ManifestDegradation {
    scope: String,
    page: usize,
    #[serde(default)]
    paragraph: Option<usize>,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct ManifestAlignment {
    page: usize,
    diagnostic: bool,
    walked_text: String,
    walked_character_count: usize,
    engine_character_count: usize,
    extraction_equivalent_count: usize,
    explained_count: usize,
    strong_unicode_conflict_count: usize,
    weak_unicode_conflict_count: usize,
    unresolved_unicode_count: usize,
    walk_only_count: usize,
    engine_only_count: usize,
    residual_count: usize,
}

fn fixture_manifest(id: &str) -> FixtureManifest {
    toml::from_str(&std::fs::read_to_string(manifest_path(id)).unwrap()).unwrap()
}

fn fixture_ids_with_case_prefixes(prefixes: &[&str]) -> BTreeSet<String> {
    std::fs::read_dir(repo_root().join("corpus/fixtures"))
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.path().join("manifest.toml").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|id| {
            fixture_manifest(id)
                .identity
                .cases
                .iter()
                .any(|case| prefixes.iter().any(|prefix| case.starts_with(prefix)))
        })
        .collect()
}

fn all_fixture_ids() -> BTreeSet<String> {
    std::fs::read_dir(repo_root().join("corpus/fixtures"))
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.path().join("manifest.toml").is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect()
}

fn snapshot_names(directory: &Path) -> Vec<String> {
    directory_names(directory)
        .into_iter()
        .filter(|name| name.ends_with(".il.json"))
        .collect()
}

fn assert_parseable_snapshots(directory: &Path, id: &str) {
    for name in snapshot_names(directory) {
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join(&name)).unwrap_or_else(|error| {
                panic!("fixture {id}: could not read snapshot {name}: {error}")
            }),
        )
        .unwrap_or_else(|error| panic!("fixture {id}: snapshot {name} is invalid: {error}"));
        assert_eq!(value["schema_version"], 1, "fixture {id}: {name}");
    }
}

fn assert_none_translation_identity(snapshot: &serde_json::Value, id: &str) {
    for page in snapshot["pages"].as_array().unwrap() {
        for paragraph in page["paragraphs"].as_array().unwrap() {
            if paragraph.get("preserved").is_some() {
                assert!(paragraph["translated_text"].is_null(), "fixture {id}");
            } else {
                assert_eq!(
                    paragraph["translated_text"].as_str(),
                    Some(il_paragraph_text(paragraph).as_str()),
                    "fixture {id}: none backend changed paragraph text"
                );
            }
        }
    }
}

fn expected_page_transforms(
    manifest: &FixtureManifest,
    page_index: usize,
) -> Vec<(String, Option<f64>)> {
    let mut blocks = manifest
        .expected
        .block
        .iter()
        .filter(|block| block.page == page_index)
        .collect::<Vec<_>>();
    blocks.sort_by_key(|block| block.draw_order);

    let mut result = Vec::new();
    for block in blocks {
        let mut block_transforms = vec![None; block.text.chars().count()];
        for expected in manifest
            .expected
            .transform
            .iter()
            .filter(|expected| expected.block == block.key)
        {
            for &char_index in &expected.char_indices {
                assert!(block_transforms[char_index].is_none());
                block_transforms[char_index] = Some((expected.kind.clone(), expected.degrees));
            }
        }
        result.extend(block_transforms.into_iter().map(|expected| {
            expected.unwrap_or_else(|| {
                panic!(
                    "manifest block {} does not declare every transform",
                    block.key
                )
            })
        }));
    }
    result
}

fn expected_page_text(manifest: &FixtureManifest, page_index: usize) -> String {
    let mut blocks = manifest
        .expected
        .block
        .iter()
        .filter(|block| block.page == page_index)
        .collect::<Vec<_>>();
    blocks.sort_by_key(|block| block.draw_order);
    blocks.iter().map(|block| block.text.as_str()).collect()
}

fn il_paragraph_text(paragraph: &serde_json::Value) -> String {
    let mut output = String::new();
    for character in paragraph["text"]["chars"].as_array().unwrap() {
        let Some(unicode) = character["unicode"].as_str() else {
            continue;
        };
        if character["implicit_space_before"] == true
            && !output.ends_with(char::is_whitespace)
            && !unicode.starts_with(char::is_whitespace)
        {
            output.push(' ');
        }
        output.push_str(unicode);
    }
    output
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
    configure_test_fonts(&mut command);
    command.env("HTTP_PROXY", "http://127.0.0.1:9");
    command.env("HTTPS_PROXY", "http://127.0.0.1:9");
    command.env("OPENAI_API_KEY", "must-not-be-used");
    if json {
        command.arg("--json");
    }
    command.args(["translate", "--backend", "none", "--layout", "single-line"]);
    if let Some(output) = output {
        command.arg(output_flag).arg(output);
    }
    command.arg(input).output().unwrap()
}

fn run_inspect(input: &Path, json: bool, debug: Option<&Path>) -> Output {
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    configure_test_fonts(&mut command);
    if json {
        command.arg("--json");
    }
    command.args(["inspect", "--layout", "single-line"]);
    if let Some(debug) = debug {
        command.arg("--debug").arg(debug);
    }
    command.arg(input).output().unwrap()
}

fn run_inspect_with_layout(id: &str) -> Output {
    run_inspect_with_recording(id, id)
}

fn run_inspect_with_recording(fixture_id: &str, recording_id: &str) -> Output {
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    command
        .args(["--json", "inspect", "--layout-replay"])
        .arg(layout_recording_path(recording_id))
        .arg(fixture_path(fixture_id))
        .output()
        .unwrap()
}

fn run_none_with_debug(input: &Path, output: &Path, debug: &Path, json: bool) -> Output {
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    configure_test_fonts(&mut command);
    if json {
        command.arg("--json");
    }
    command
        .args([
            "translate",
            "--backend",
            "none",
            "--layout",
            "single-line",
            "--output",
        ])
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

fn scan_summary(events: &[serde_json::Value]) -> Option<serde_json::Value> {
    events
        .iter()
        .find(|event| event["event"] == "diagnostic" && event["id"] == "scan_summary")
        .cloned()
}

fn assert_scan_summary(
    summary: &serde_json::Value,
    indices: &[usize],
    scanned: usize,
    blank: usize,
    content: usize,
    total: usize,
) {
    assert_eq!(summary["scanned_page_indices"], serde_json::json!(indices));
    assert_eq!(summary["scanned_pages"], scanned);
    assert_eq!(summary["blank_pages"], blank);
    assert_eq!(summary["content_pages"], content);
    assert_eq!(summary["total_pages"], total);
}

#[derive(Clone, Copy)]
struct ScanExpectation<'a> {
    indices: &'a [usize],
    scanned: usize,
    blank: usize,
    content: usize,
    total: usize,
}

#[derive(Clone, Copy)]
struct ContinuationCase<'a> {
    id: &'a str,
    summary: Option<ScanExpectation<'a>>,
    passthrough_indices: &'a [usize],
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

fn write_rotated_pdf(directory: &Path, name: &str, rotate: i64) -> PathBuf {
    let path = directory.join(name);
    let mut document = lopdf::Document::load(fixture()).unwrap();
    let page_id = document.get_pages()[&1];
    document
        .get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Rotate", rotate);
    document.save(&path).unwrap();
    path
}

fn decoded_page_streams(path: &Path, page_number: u32) -> Vec<Vec<u8>> {
    let document = lopdf::Document::load(path).unwrap();
    let page_id = document.get_pages()[&page_number];
    document
        .get_page_contents(page_id)
        .into_iter()
        .map(|(object, generation)| {
            let output = Command::new("qpdf")
                .arg(format!("--show-object={object} {generation} R"))
                .arg("--filtered-stream-data")
                .arg(path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        })
        .collect()
}

fn page_content_ids(path: &Path, page_number: u32) -> Vec<(u32, u16)> {
    let document = lopdf::Document::load(path).unwrap();
    let page_id = document.get_pages()[&page_number];
    document.get_page_contents(page_id)
}

fn local_page_entry(path: &Path, page_number: u32, key: &[u8]) -> Option<lopdf::Object> {
    let document = lopdf::Document::load(path).unwrap();
    let page_id = document.get_pages()[&page_number];
    document
        .get_object(page_id)
        .unwrap()
        .as_dict()
        .unwrap()
        .get(key)
        .ok()
        .cloned()
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

    let output = Command::new(BIN)
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--concurrency",
            "0",
        ])
        .arg(fixture())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "error");
    assert_eq!(events[0]["reason"], "invalid_arguments");
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
fn default_openai_backend_requires_a_key_without_exposing_a_key_flag() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("paper.pdf");
    std::fs::copy(fixture(), &input).unwrap();
    let output = Command::new(BIN)
        .env_remove(PDFIUM_ENV)
        .env("MIMUS_CONFIG_FILE", directory.path().join("missing.toml"))
        .env("API_KEY", "")
        .env_remove("MIMUS_OPENAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .args(["translate", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid_arguments"));
    assert!(stderr.contains("API_KEY"));
    assert!(!stderr.contains("--api-key"));
    assert!(!directory.path().join("paper.zh.pdf").exists());
}

#[test]
fn missing_explicit_layout_model_fails_before_pdfium_as_asset_exit_three() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(BIN)
        .current_dir(directory.path())
        .env_remove(PDFIUM_ENV)
        .env("MIMUS_CONFIG_FILE", directory.path().join("missing.toml"))
        .args(["--json", "inspect", "--layout-model"])
        .arg(directory.path().join("missing.onnx"))
        .arg(fixture())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "error");
    assert_eq!(events.last().unwrap()["category"], "asset");
    assert_eq!(events.last().unwrap()["reason"], "layout_model_unavailable");
}

#[test]
fn single_line_layout_is_an_explicit_offline_degradation_mode() {
    let output = Command::new(BIN)
        .env(PDFIUM_ENV, pdfium_library())
        .args(["--json", "inspect", "--layout", "single-line"])
        .arg(fixture())
        .output()
        .unwrap();

    assert!(output.status.success());
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "result");
}

#[test]
fn translation_config_resolves_each_non_secret_field_flag_then_env_then_file() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        "backend = 'none'\nbase_url = 'https://file.invalid'\nmodel = 'file-model'\ntarget_language = 'file-language'\nfont_regular = 'file-regular.ttf'\nfont_bold = 'file-bold.ttf'\nfont_fallback_regular = 'file-fallback-regular.ttf'\nfont_fallback_bold = 'file-fallback-bold.ttf'\ncache = 'file-cache.redb'\nconcurrency = 2\n",
    )
    .unwrap();
    let output_path = directory.path().join("translated.pdf");
    let output = Command::new(BIN)
        .env(PDFIUM_ENV, pdfium_library())
        .env("MIMUS_CONFIG_FILE", &config)
        .env("BASE_URL", "https://env.invalid")
        .env("MODEL_ID", "env-model")
        .env("TARGET_LANGUAGE", "env-language")
        .env("MIMUS_CACHE", "env-cache.redb")
        .env("MIMUS_CONCURRENCY", "invalid-but-overridden")
        .env("MIMUS_BACKEND", "invalid-but-overridden")
        .env("MIMUS_FONT_REGULAR", "env-regular.ttf")
        .env("MIMUS_FONT_BOLD", "env-bold.ttf")
        .env("MIMUS_FONT_FALLBACK_REGULAR", "env-fallback-regular.ttf")
        .env("MIMUS_FONT_FALLBACK_BOLD", "env-fallback-bold.ttf")
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--endpoint",
            "http://flag.invalid",
            "--model",
            "flag-model",
            "--target-language",
            "flag-language",
            "--cache",
            "flag-cache.redb",
            "--concurrency",
            "5",
            "--layout",
            "single-line",
        ])
        .arg("--font")
        .arg(test_font_path("Regular"))
        .arg("--font-bold")
        .arg(test_font_path("Bold"))
        .arg("--font-fallback")
        .arg(test_fallback_font_path("Regular"))
        .arg("--font-fallback-bold")
        .arg(test_fallback_font_path("Bold"))
        .arg("--output")
        .arg(&output_path)
        .arg(fixture())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_events(&output.stdout);
    let resolved = events
        .iter()
        .find(|event| event["event"] == "configuration_resolved")
        .unwrap();
    assert_eq!(resolved["backend"], "none");
    assert_eq!(resolved["endpoint"], "http://flag.invalid");
    assert_eq!(resolved["model"], "flag-model");
    assert_eq!(resolved["target_language"], "flag-language");
    assert!(
        resolved["font_regular_source"]
            .as_str()
            .unwrap()
            .starts_with("flag:")
    );
    assert_eq!(
        resolved["font_regular_sha256"],
        "510d0470ca8b77f035fe8e7143526207088c1bdad017451cf253020f72397d63"
    );
    assert!(
        resolved["font_bold_source"]
            .as_str()
            .unwrap()
            .starts_with("flag:")
    );
    assert_eq!(
        resolved["font_bold_sha256"],
        "1a917349eb06866f5701532f0cea586d184edadbd1cfdd3f034f3a18f2ff5316"
    );
    assert!(
        resolved["font_fallback_regular_source"]
            .as_str()
            .unwrap()
            .starts_with("flag:")
    );
    assert_eq!(
        resolved["font_fallback_regular_sha256"],
        "3634d4b65a151c61dcb82968f6a3bdc33435d062c4c69a5ea57e3db20122ac1e"
    );
    assert!(
        resolved["font_fallback_bold_source"]
            .as_str()
            .unwrap()
            .starts_with("flag:")
    );
    assert_eq!(
        resolved["font_fallback_bold_sha256"],
        "d0f2fdc62e7cdf6e35c8b0629b19084917991603c0d51fe94109128176352b83"
    );
    assert_eq!(resolved["cache_enabled"], true);
    assert_eq!(resolved["cache_path"], "flag-cache.redb");
    assert_eq!(resolved["concurrency"], 5);
    assert_eq!(resolved["layout_mode"], "single_line");
    assert!(resolved.get("layout_model_source").is_none());
    assert!(resolved.get("layout_model_sha256").is_none());
    assert!(events.iter().all(|event| event.get("api_key").is_none()));
}

#[test]
fn secret_bearing_endpoints_are_rejected_before_configuration_is_emitted() {
    let directory = tempfile::tempdir().unwrap();
    let endpoint_canary = "endpoint-secret-canary";
    let endpoint =
        format!("https://user:{endpoint_canary}@example.test/v1?token={endpoint_canary}");
    let output = Command::new(BIN)
        .current_dir(directory.path())
        .env("API_KEY", "api-key-canary")
        .env_remove("MIMUS_OPENAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .args(["--json", "translate", "--endpoint", &endpoint])
        .arg(fixture())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!rendered.contains(endpoint_canary));
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "error");
    assert!(
        events
            .iter()
            .all(|event| event["event"] != "configuration_resolved")
    );
}

#[test]
fn empty_secret_alias_falls_through_to_the_next_nonempty_alias() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(BIN)
        .current_dir(directory.path())
        .env_remove(PDFIUM_ENV)
        .env("MIMUS_OPENAI_API_KEY", "")
        .env_remove("OPENAI_API_KEY")
        .env("API_KEY", "fallback-secret-canary")
        .env("BASE_URL", "http://127.0.0.1:9")
        .env("MODEL_ID", "test-model")
        .env("MIMUS_FONT_REGULAR", test_font_path("Regular"))
        .env("MIMUS_FONT_BOLD", test_font_path("Bold"))
        .env(
            "MIMUS_FONT_FALLBACK_REGULAR",
            test_fallback_font_path("Regular"),
        )
        .env("MIMUS_FONT_FALLBACK_BOLD", test_fallback_font_path("Bold"))
        .args(["--json", "translate", "--layout", "single-line"])
        .arg(fixture())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!rendered.contains("fallback-secret-canary"));
    assert!(!rendered.contains("API key is required"));
}

#[test]
fn missing_output_fonts_fail_fast_as_asset_without_contacting_a_public_endpoint() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(BIN)
        .env("MIMUS_CONFIG_FILE", directory.path().join("missing.toml"))
        .env("MIMUS_CACHE_DIR", directory.path().join("cache"))
        .env("MIMUS_ASSET_MIRROR", "http://127.0.0.1:9")
        .env_remove("MIMUS_FONT_REGULAR")
        .env_remove("MIMUS_FONT_BOLD")
        .env_remove("MIMUS_FONT_FALLBACK_REGULAR")
        .env_remove("MIMUS_FONT_FALLBACK_BOLD")
        .args(["--json", "translate", "--backend", "none"])
        .arg(fixture())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "error");
    assert_eq!(events.last().unwrap()["reason"], "output_font_unavailable");
    assert!(
        events.last().unwrap()["hint"]
            .as_str()
            .unwrap()
            .contains("--font")
    );
}

#[test]
fn no_cache_resolves_as_a_complete_read_write_bypass() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let output = command
        .env(PDFIUM_ENV, pdfium_library())
        .env("MIMUS_CACHE", directory.path().join("environment.redb"))
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--no-cache",
            "--layout",
            "single-line",
            "--output",
        ])
        .arg(&output_path)
        .arg(fixture())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_events(&output.stdout);
    let resolved = events
        .iter()
        .find(|event| event["event"] == "configuration_resolved")
        .unwrap();
    assert_eq!(resolved["cache_enabled"], false);
    assert!(resolved["cache_path"].is_null());
    assert!(
        events
            .iter()
            .all(|event| event["event"] != "translation_cache")
    );
    assert!(!directory.path().join("environment.redb").exists());
}

#[test]
fn missing_and_malformed_glossaries_fail_as_usage_before_pdf_or_network_work() {
    let directory = tempfile::tempdir().unwrap();
    let malformed = directory.path().join("malformed.toml");
    std::fs::write(
        &malformed,
        "version = 1\n[[terms]]\nsource = ''\ntarget = 'x'\n",
    )
    .unwrap();
    for glossary in [directory.path().join("missing.toml"), malformed] {
        let output = Command::new(BIN)
            .env_remove(PDFIUM_ENV)
            .env(
                "MIMUS_CONFIG_FILE",
                directory.path().join("config-missing.toml"),
            )
            .args(["translate", "--backend", "none", "--glossary"])
            .arg(&glossary)
            .arg(directory.path().join("input-does-not-exist.pdf"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_arguments"));
    }
}

#[test]
fn user_glossary_dumps_as_a_stable_round_trip_when_auto_terms_are_disabled() {
    let directory = tempfile::tempdir().unwrap();
    let glossary = directory.path().join("user.toml");
    let dumped = directory.path().join("dumped.toml");
    let output_pdf = directory.path().join("translated.pdf");
    std::fs::write(
        &glossary,
        "version = 1\n[[terms]]\nsource = 'zeta'\ntarget = 'z'\n[[terms]]\nsource = 'alpha'\ntarget = 'a'\n",
    )
    .unwrap();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let output = command
        .env(PDFIUM_ENV, pdfium_library())
        .env(
            "MIMUS_CONFIG_FILE",
            directory.path().join("config-missing.toml"),
        )
        .args([
            "translate",
            "--backend",
            "none",
            "--no-auto-terms",
            "--layout",
            "single-line",
            "--glossary",
        ])
        .arg(&glossary)
        .arg("--dump-glossary")
        .arg(&dumped)
        .arg("--output")
        .arg(&output_pdf)
        .arg(fixture())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = mimus_core::translate::Glossary::from_path(&glossary).unwrap();
    let round_trip = mimus_core::translate::Glossary::from_path(&dumped).unwrap();
    assert_eq!(round_trip, original);
    assert_eq!(round_trip.fingerprint(), original.fingerprint());
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
fn m1_corpus_inventory_runs_every_fixture_through_bounded_production_paths() {
    let ids = all_fixture_ids();
    let cases = ids
        .iter()
        .flat_map(|id| fixture_manifest(id).identity.cases)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 149, "M1 closure fixture inventory changed");
    assert_eq!(cases.len(), 87, "M1 closure case inventory changed");

    for id in ids {
        let input = fixture_path(&id);
        let directory = tempfile::tempdir().unwrap();

        let inspect_debug = directory.path().join("inspect-debug");
        let inspected = run_inspect(&input, true, Some(&inspect_debug));
        let inspect_code = inspected
            .status
            .code()
            .unwrap_or_else(|| panic!("fixture {id}: inspect terminated by signal"));
        assert!(inspected.stderr.is_empty(), "fixture {id}: inspect stderr");
        let inspect_events = parse_events(&inspected.stdout);
        assert_one_terminal_last(
            &inspect_events,
            if inspect_code == 0 { "result" } else { "error" },
        );
        assert!(
            matches!(inspect_code, 0 | 2),
            "fixture {id}: unexpected inspect exit code {inspect_code}"
        );
        assert_parseable_snapshots(&inspect_debug, &id);
        if inspect_code == 0 {
            assert_eq!(
                snapshot_names(&inspect_debug),
                vec![
                    "00-parse.il.json",
                    "01-scan_detect.il.json",
                    "02-layout.il.json",
                    "03-paragraph_find.il.json",
                ],
                "fixture {id}: incomplete inspect snapshots"
            );
            let terminal = inspect_events.last().unwrap();
            assert_eq!(terminal["il"]["schema_version"], 1, "fixture {id}");
            assert_eq!(
                terminal["pages"].as_u64(),
                Some(fixture_manifest(&id).page.len() as u64),
                "fixture {id}"
            );
        } else {
            assert_eq!(
                inspect_events.last().unwrap()["category"],
                "input",
                "fixture {id}"
            );
        }

        let translate_debug = directory.path().join("translate-debug");
        let translated = directory.path().join("translated.pdf");
        let translated_result = run_none_with_debug(&input, &translated, &translate_debug, true);
        let translate_code = translated_result
            .status
            .code()
            .unwrap_or_else(|| panic!("fixture {id}: translate terminated by signal"));
        assert!(
            translated_result.stderr.is_empty(),
            "fixture {id}: translate stderr"
        );
        let translate_events = parse_events(&translated_result.stdout);
        assert_one_terminal_last(
            &translate_events,
            if translate_code == 0 {
                "result"
            } else {
                "error"
            },
        );
        assert!(
            matches!(translate_code, 0 | 2),
            "fixture {id}: unexpected translate exit code {translate_code}"
        );
        assert_parseable_snapshots(&translate_debug, &id);

        if translate_code == 0 {
            assert_eq!(
                snapshot_names(&translate_debug),
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
                ],
                "fixture {id}: incomplete translate snapshots"
            );
            let translate_snapshot: serde_json::Value = serde_json::from_slice(
                &std::fs::read(translate_debug.join("06-translate.il.json")).unwrap(),
            )
            .unwrap();
            assert_none_translation_identity(&translate_snapshot, &id);
            assert!(translated.is_file(), "fixture {id}: no translated output");
            let input_bytes = std::fs::read(&input).unwrap();
            let output_bytes = std::fs::read(&translated).unwrap();
            assert!(output_bytes.starts_with(&input_bytes), "fixture {id}");
            let qpdf = Command::new("qpdf")
                .arg("--check")
                .arg(&translated)
                .output()
                .unwrap();
            let legality = fixture_manifest(&id).identity.legality;
            if legality == "legal" {
                assert!(
                    qpdf.status.success(),
                    "fixture {id}: {}",
                    String::from_utf8_lossy(&qpdf.stderr)
                );
            } else {
                assert_eq!(legality, "malformed", "fixture {id}");
                assert!(
                    matches!(qpdf.status.code(), Some(0 | 3)),
                    "fixture {id}: {}",
                    String::from_utf8_lossy(&qpdf.stderr)
                );
            }
        } else {
            assert_eq!(
                translate_events.last().unwrap()["category"],
                "input",
                "fixture {id}"
            );
            assert!(!translated.exists(), "fixture {id}: failure wrote output");
        }
    }
}

#[test]
fn alignment_fixture_classifications_match_manifest_through_production() {
    let ids = fixture_ids_with_case_prefixes(&["ALIGN-"]);
    assert_eq!(ids.len(), 9);

    for id in ids {
        let manifest = fixture_manifest(&id);
        assert!(!manifest.expected.alignment.is_empty(), "fixture {id}");
        let input = fixture_path(&id);
        let inspected = run_inspect(&input, true, None);
        assert!(
            inspected.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        let events = parse_events(&inspected.stdout);
        assert_one_terminal_last(&events, "result");
        let result = events.last().unwrap();
        let diagnostics = events
            .iter()
            .filter(|event| {
                event["event"] == "diagnostic" && event["id"] == "engine_character_alignment"
            })
            .collect::<Vec<_>>();
        assert_eq!(
            diagnostics.len(),
            manifest
                .expected
                .alignment
                .iter()
                .filter(|expected| expected.diagnostic)
                .count(),
            "fixture {id}"
        );

        for expected in &manifest.expected.alignment {
            let actual = diagnostics
                .iter()
                .copied()
                .find(|event| event["page_index"].as_u64() == Some(expected.page as u64));
            if !expected.diagnostic {
                assert!(actual.is_none(), "fixture {id}, page {}", expected.page);
            } else {
                let actual = actual.unwrap_or_else(|| {
                    panic!(
                        "fixture {id}, page {} has no alignment diagnostic",
                        expected.page
                    )
                });
                assert_eq!(
                    actual,
                    &serde_json::json!({
                        "schema_version": 2,
                        "event": "diagnostic",
                        "id": "engine_character_alignment",
                        "page_index": expected.page,
                        "walked_character_count": expected.walked_character_count,
                        "engine_character_count": expected.engine_character_count,
                        "extraction_equivalent_count": expected.extraction_equivalent_count,
                        "explained_count": expected.explained_count,
                        "strong_unicode_conflict_count": expected.strong_unicode_conflict_count,
                        "weak_unicode_conflict_count": expected.weak_unicode_conflict_count,
                        "unresolved_unicode_count": expected.unresolved_unicode_count,
                        "walk_only_count": expected.walk_only_count,
                        "engine_only_count": expected.engine_only_count,
                        "residual_count": expected.residual_count,
                    }),
                    "fixture {id}, page {}",
                    expected.page
                );
            }

            let walked_text = result["il"]["pages"][expected.page]["paragraphs"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|paragraph| paragraph["text"]["chars"].as_array().unwrap())
                .filter_map(|character| character["unicode"].as_str())
                .collect::<String>();
            assert_eq!(walked_text, expected.walked_text, "fixture {id}");
        }

        let expected_preserved = manifest
            .expected
            .degradation
            .iter()
            .filter(|degradation| degradation.scope == "paragraph")
            .map(|degradation| {
                (
                    degradation.page,
                    degradation.paragraph.unwrap(),
                    degradation.reason.as_str(),
                )
            })
            .collect::<BTreeSet<_>>();
        let actual_preserved = result["il"]["pages"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .flat_map(|(page_index, page)| {
                page["paragraphs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .filter_map(move |(paragraph_index, paragraph)| {
                        paragraph["preserved"]
                            .as_str()
                            .map(|reason| (page_index, paragraph_index, reason))
                    })
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(actual_preserved, expected_preserved, "fixture {id}");

        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join(format!("{id}.pdf"));
        let translated = run_none(&input, Some(&output), true);
        assert!(
            translated.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&translated.stderr)
        );
        let output_bytes = std::fs::read(&output).unwrap();
        assert!(
            !output_bytes
                .windows(b"(cid:".len())
                .any(|window| window == b"(cid:"),
            "fixture {id}"
        );
        if !expected_preserved.is_empty() {
            assert_eq!(output_bytes, std::fs::read(&input).unwrap(), "fixture {id}");
        }
    }
}

#[test]
fn scan_rejection_matrix_matches_for_inspect_and_translate() {
    let cases = [
        ("unit-scan-01-image-only", vec![0], 1, 0, 1, 1),
        ("unit-scan-02-invisible-ocr", vec![0], 1, 0, 1, 1),
        (
            "intg-scan-10-nine-of-ten",
            (0..9).collect::<Vec<_>>(),
            9,
            0,
            10,
            10,
        ),
        (
            "intg-scan-11-four-of-five",
            (0..4).collect::<Vec<_>>(),
            4,
            0,
            5,
            5,
        ),
        ("intg-scan-12-image-with-blank-backs", vec![0], 1, 9, 1, 10),
    ];
    let directory = tempfile::tempdir().unwrap();

    for (id, indices, scanned, blank, content, total) in cases {
        let input = fixture_path(id);
        let output_path = directory.path().join(format!("{id}-output.pdf"));
        let inspect = run_inspect(&input, true, None);
        let translate = run_none(&input, Some(&output_path), true);

        for command in [&inspect, &translate] {
            assert_eq!(command.status.code(), Some(2), "fixture {id}");
            assert!(command.stderr.is_empty(), "fixture {id}");
            let events = parse_events(&command.stdout);
            assert_one_terminal_last(&events, "error");
            let summary = scan_summary(&events).unwrap();
            assert_scan_summary(&summary, &indices, scanned, blank, content, total);
            let error = events.last().unwrap();
            assert_eq!(error["category"], "input");
            assert_eq!(error["reason"], "scanned_pdf");
            assert_eq!(error["scanned_pages"], scanned);
            assert_eq!(error["total_pages"], total);
            assert_eq!(
                error["message"],
                format!("{scanned} of {content} content pages are scanned")
            );
            assert!(error["hint"].as_str().unwrap().contains("OCR"));
        }
        assert_eq!(
            scan_summary(&parse_events(&inspect.stdout)),
            scan_summary(&parse_events(&translate.stdout)),
            "inspect and translate disagreed for {id}"
        );
        assert!(!output_path.exists(), "fixture {id} produced output");
    }
}

#[test]
fn scan_continuation_matrix_preserves_passthrough_pages() {
    let cases = [
        ContinuationCase {
            id: "unit-scan-04-title-page",
            summary: None,
            passthrough_indices: &[],
        },
        ContinuationCase {
            id: "intg-scan-06-blank-middle",
            summary: None,
            passthrough_indices: &[1],
        },
        ContinuationCase {
            id: "intg-scan-07-image-middle",
            summary: Some(ScanExpectation {
                indices: &[1],
                scanned: 1,
                blank: 0,
                content: 3,
                total: 3,
            }),
            passthrough_indices: &[1],
        },
        ContinuationCase {
            id: "intg-scan-08-text-first",
            summary: Some(ScanExpectation {
                indices: &[1, 2, 3],
                scanned: 3,
                blank: 0,
                content: 4,
                total: 4,
            }),
            passthrough_indices: &[1, 2, 3],
        },
        ContinuationCase {
            id: "intg-scan-09-text-last",
            summary: Some(ScanExpectation {
                indices: &[0, 1, 2],
                scanned: 3,
                blank: 0,
                content: 4,
                total: 4,
            }),
            passthrough_indices: &[0, 1, 2],
        },
    ];
    let directory = tempfile::tempdir().unwrap();

    for case in cases {
        let input = fixture_path(case.id);
        let output_path = directory.path().join(format!("{}-output.pdf", case.id));
        let inspect = run_inspect(&input, true, None);
        let translate = run_none(&input, Some(&output_path), true);

        assert!(inspect.status.success(), "inspect {}", case.id);
        assert!(translate.status.success(), "translate {}", case.id);
        let inspect_events = parse_events(&inspect.stdout);
        let translate_events = parse_events(&translate.stdout);
        assert_one_terminal_last(&inspect_events, "result");
        assert_one_terminal_last(&translate_events, "result");
        assert_eq!(
            scan_summary(&inspect_events),
            scan_summary(&translate_events)
        );
        let expected_warnings = usize::from(case.summary.is_some());
        assert_eq!(
            inspect_events.last().unwrap()["warnings"],
            expected_warnings
        );
        assert_eq!(
            translate_events.last().unwrap()["warnings"],
            expected_warnings
        );
        match case.summary {
            Some(summary) => assert_scan_summary(
                &scan_summary(&inspect_events).unwrap(),
                summary.indices,
                summary.scanned,
                summary.blank,
                summary.content,
                summary.total,
            ),
            None => assert!(scan_summary(&inspect_events).is_none()),
        }
        let il_pages = inspect_events.last().unwrap()["il"]["pages"]
            .as_array()
            .unwrap();
        for index in case.passthrough_indices {
            assert_eq!(il_pages[*index]["paragraphs"], serde_json::json!([]));
        }
        assert!(output_path.is_file());

        if case.id == "intg-scan-07-image-middle" {
            let input_pages = qpdf_pages(&input);
            let output_pages = qpdf_pages(&output_path);
            assert_eq!(output_pages[1], input_pages[1]);
            let page_object = input_pages[1]["object"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap();
            assert_eq!(
                qpdf_object(&input, page_object),
                qpdf_object(&output_path, page_object)
            );
        }
    }

    let human_output = directory.path().join("human-warning.pdf");
    let human = run_none(
        &fixture_path("intg-scan-07-image-middle"),
        Some(&human_output),
        false,
    );
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stderr).contains("warning[scan_summary]"));
}

#[test]
fn native_image_text_and_hidden_watermark_continue_as_content_pages() {
    let directory = tempfile::tempdir().unwrap();
    for (id, supported) in [
        ("unit-scan-03-visible-image-text", true),
        // The hidden baseline is an isolated passthrough unit, so the visible text remains usable.
        ("unit-scan-05-hidden-watermark", true),
    ] {
        let input = fixture_path(id);
        let output_path = directory.path().join(format!("{id}-output.pdf"));
        for command in [
            run_inspect(&input, true, None),
            run_none(&input, Some(&output_path), true),
        ] {
            assert_eq!(
                command.status.code(),
                Some(if supported { 0 } else { 2 }),
                "fixture {id}"
            );
            let events = parse_events(&command.stdout);
            assert_one_terminal_last(&events, if supported { "result" } else { "error" });
            assert!(scan_summary(&events).is_none());
            if !supported {
                assert_eq!(events.last().unwrap()["reason"], "unsupported_pdf");
            }
        }
        if supported {
            assert!(output_path.exists());
            assert_eq!(
                decoded_page_streams(&output_path, 1),
                decoded_page_streams(&input, 1),
                "fixture {id}"
            );
        } else {
            assert!(!output_path.exists());
        }
    }
}

#[test]
fn encrypted_fixture_matrix_rejects_before_output_and_keeps_empty_password_guard() {
    let directory = tempfile::tempdir().unwrap();
    for id in [
        "unit-doc-03-rc4-empty-password",
        "unit-doc-03-aes128-user-password",
    ] {
        let input = fixture_path(id);
        let output_path = directory.path().join(format!("{id}-output.pdf"));
        for command in [
            run_inspect(&input, true, None),
            run_none(&input, Some(&output_path), true),
        ] {
            assert_eq!(command.status.code(), Some(2), "fixture {id}");
            let events = parse_events(&command.stdout);
            assert_one_terminal_last(&events, "error");
            let error = events.last().unwrap();
            assert_eq!(error["reason"], "encrypted_pdf");
            assert!(error["hint"].as_str().unwrap().contains("qpdf"));
            assert!(error.get("scanned_pages").is_none());
            assert!(error.get("total_pages").is_none());
        }
        assert!(!output_path.exists());
    }

    let empty_password =
        lopdf::Document::load(fixture_path("unit-doc-03-rc4-empty-password")).unwrap();
    assert!(empty_password.was_encrypted());
    assert!(!empty_password.is_encrypted());

    let nonempty_password =
        lopdf::Document::load(fixture_path("unit-doc-03-aes128-user-password")).unwrap();
    assert!(!nonempty_password.was_encrypted());
    assert!(nonempty_password.is_encrypted());
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
fn multi_region_none_roundtrip_remains_bounded_before_paragraph_reconstruction() {
    let directory = tempfile::tempdir().unwrap();
    let id = "unit-base-02-two-column";
    let translated = directory.path().join(format!("{id}.pdf"));
    let result = run_none(&fixture_path(id), Some(&translated), false);
    assert!(
        result.status.success(),
        "fixture {id}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        decoded_page_streams(&translated, 1),
        decoded_page_streams(&fixture_path(id), 1)
    );
}

#[test]
fn recorded_layout_policy_drives_production_il_candidates_and_passthrough() {
    let expected = [
        (
            "unit-order-01-natural",
            "pp-doclayoutv3-unit-order-01-natural",
            (
                expected_page_text(&fixture_manifest("unit-order-01-natural"), 0),
                String::new(),
            ),
        ),
        (
            "unit-layout-01-nested-boxes",
            "unit-layout-01-nested-boxes",
            (
                "Body text ends here, two points shy of the frame.".to_owned(),
                "Table 1. Throughput measured over ten runs.".to_owned(),
            ),
        ),
        (
            "unit-layout-07-policy-zones",
            "unit-layout-07-policy-zones",
            (
                concat!(
                    "The first body paragraph is the only kind of text on this page that a ",
                    "translator should ever see. Everything around it belongs to a policy zone.",
                    "The second body paragraph is likewise ordinary prose. Between them the page ",
                    "carries a running head, a folio, a reference entry and a seal."
                )
                .to_owned(),
                concat!(
                    "Journal of Reproducible Layout, Vol. 3",
                    "[1] Smith et al. Layout preservation in machine translation. 2024.",
                    "APPROVED",
                    "17"
                )
                .to_owned(),
            ),
        ),
        (
            "unit-layout-02-table-only",
            "unit-layout-02-table-only",
            (
                String::new(),
                "RunThroughputLatencyfirst1204 ops8.1 mssecond1198 ops8.3 ms".to_owned(),
            ),
        ),
        (
            "unit-layout-08-narrow-gutter",
            "unit-layout-08-narrow-gutter",
            (
                String::new(),
                expected_page_text(&fixture_manifest("unit-layout-08-narrow-gutter"), 0),
            ),
        ),
    ];

    for (id, recording_id, (expected_translate, expected_passthrough)) in expected {
        let output = run_inspect_with_recording(id, recording_id);
        assert!(
            output.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = parse_events(&output.stdout);
        assert_one_terminal_last(&events, "result");
        let chars = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|paragraph| paragraph["text"]["chars"].as_array().unwrap())
            .collect::<Vec<_>>();
        let collect_policy = |policy: &str| {
            let mut selected = chars
                .iter()
                .filter(|character| character["layout"]["policy"] == policy)
                .collect::<Vec<_>>();
            selected.sort_by_key(|character| {
                (
                    character["passthrough"]["content_object"].as_u64(),
                    character["passthrough"]["byte_start"].as_u64(),
                    character["passthrough"]["byte_end"].as_u64(),
                )
            });
            selected
                .into_iter()
                .filter_map(|character| character["unicode"].as_str())
                .collect::<String>()
        };
        assert_eq!(
            collect_policy("translate"),
            expected_translate,
            "fixture {id}"
        );
        assert_eq!(
            collect_policy("passthrough"),
            expected_passthrough,
            "fixture {id}"
        );
    }

    let policy = run_inspect_with_layout("unit-layout-07-policy-zones");
    let events = parse_events(&policy.stdout);
    let chars = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|paragraph| paragraph["text"]["chars"].as_array().unwrap())
        .collect::<Vec<_>>();
    let labels = chars
        .iter()
        .filter_map(|character| character["layout"]["label"].as_str())
        .collect::<BTreeSet<_>>();
    assert!(labels.contains("header"));
    assert!(labels.contains("reference_content"));
    assert!(labels.contains("seal"));
    assert!(labels.contains("number"));
}

#[test]
fn table_translation_is_experimental_reported_and_off_without_remote_calls() {
    let help = Command::new(BIN)
        .args(["translate", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("--translate-table"));
    assert!(help.contains("Experimental:"));

    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("default-table.pdf");
    let server = FakeResponsesServer::start();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let output = command
        .env(PDFIUM_ENV, pdfium_library())
        .env("API_KEY", "mimus-table-test-key")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--json",
            "translate",
            "--backend",
            "openai",
            "--endpoint",
            &server.endpoint,
            "--model",
            "table-test-model",
            "--no-cache",
            "--strict",
            "--layout-replay",
        ])
        .arg(layout_recording_path("unit-layout-02-table-only"))
        .arg("--output")
        .arg(&output_path)
        .arg(fixture_path("unit-layout-02-table-only"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(server.requests().is_empty());
    assert_eq!(
        std::fs::read(&output_path).unwrap(),
        std::fs::read(fixture_path("unit-layout-02-table-only")).unwrap()
    );
    let events = parse_events(&output.stdout);
    let resolved = events
        .iter()
        .find(|event| event["event"] == "configuration_resolved")
        .unwrap();
    assert_eq!(resolved["translate_table"], false);
    assert_eq!(events.last().unwrap()["translate_table"], false);
}

#[test]
fn enabled_table_translation_uses_cells_and_preserves_only_the_failed_cell() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated-table.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let output = command
        .env(PDFIUM_ENV, pdfium_library())
        .env("API_KEY", "mimus-table-test-key")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--json",
            "translate",
            "--backend",
            "openai",
            "--endpoint",
            &server.endpoint,
            "--model",
            "table-test-model",
            "--no-auto-terms",
            "--no-cache",
            "--translate-table",
            "--layout-replay",
        ])
        .arg(layout_recording_path("unit-layout-02-table-only"))
        .arg("--debug")
        .arg(&debug)
        .arg("--output")
        .arg(&output_path)
        .arg(fixture_path("unit-layout-02-table-only"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    assert!(output_path.is_file());
    let mut requests = server.requests();
    requests.sort();
    let mut expected = vec![
        "Run",
        "Throughput",
        "Latency",
        "first",
        "1204 ops",
        "8.1 ms",
        "second",
        "1198 ops",
        "8.3 ms",
    ];
    expected.sort_unstable();
    assert_eq!(requests, expected);

    let events = parse_events(&output.stdout);
    let resolved = events
        .iter()
        .find(|event| event["event"] == "configuration_resolved")
        .unwrap();
    assert_eq!(resolved["translate_table"], true);
    assert_eq!(events.last().unwrap()["translate_table"], true);
    let summary = events
        .iter()
        .find(|event| event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["preserved_paragraph_count"], 1, "{summary}");
    assert_eq!(
        summary["preserved_paragraphs"][0]["reason"],
        "translation_failure"
    );

    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(debug.join("06-translate.il.json")).unwrap())
            .unwrap();
    let paragraphs = snapshot["pages"][0]["paragraphs"].as_array().unwrap();
    assert_eq!(paragraphs.len(), 9);
    assert!(paragraphs.iter().all(|paragraph| {
        paragraph["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| {
                character["layout"]["label"] == "table"
                    && character["layout"]["policy"] == "translate"
            })
    }));
    let preserved = paragraphs
        .iter()
        .filter(|paragraph| paragraph.get("preserved").is_some())
        .collect::<Vec<_>>();
    assert_eq!(preserved.len(), 1);
    assert_eq!(il_paragraph_text(preserved[0]), "first");
    assert_eq!(preserved[0]["preserved"], "translation_failure");
    assert!(preserved[0]["translated_text"].is_null());
    assert!(
        paragraphs
            .iter()
            .filter(|paragraph| paragraph.get("preserved").is_none())
            .all(|paragraph| paragraph["translated_text"] == "M")
    );
}

#[test]
fn paragraph_reconstruction_matches_manifest_order_and_candidate_text() {
    for id in [
        "unit-base-02-two-column",
        "unit-order-01-natural",
        "unit-order-02-reversed",
        "unit-order-03-interleaved",
        "unit-order-04-column-continuation",
        "unit-order-05-false-jump",
        "unit-order-06-cross-page",
        "unit-para-07-line-numbers",
    ] {
        let manifest = fixture_manifest(id);
        let output = run_inspect(&fixture_path(id), true, None);
        assert!(
            output.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = parse_events(&output.stdout);
        let pages = events.last().unwrap()["il"]["pages"].as_array().unwrap();
        for (page_index, page) in pages.iter().enumerate() {
            let mut expected = manifest
                .expected
                .block
                .iter()
                .filter(|block| block.page == page_index)
                .collect::<Vec<_>>();
            expected.sort_by_key(|block| block.reading_order);
            let actual = page["paragraphs"]
                .as_array()
                .unwrap()
                .iter()
                .map(il_paragraph_text)
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                expected
                    .iter()
                    .map(|block| block.text.clone())
                    .collect::<Vec<_>>(),
                "fixture {id}, page {page_index}"
            );
            assert_eq!(
                page["paragraphs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|paragraph| paragraph["reading_order"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                (0..u64::try_from(expected.len()).unwrap()).collect::<Vec<_>>(),
                "fixture {id}, page {page_index}"
            );
        }
    }

    let id = "unit-para-04-toc";
    let expected = [
        "1 Introduction........................................3",
        "1.1 Background........7",
        "2 Method⋯⋯⋯⋯⋯⋯⋯⋯12",
        "2.1 Setup18",
        "3 Results············24",
        "4 Conclusion31",
    ];
    let manifest = fixture_manifest(id);
    assert_eq!(expected.join(" "), manifest.expected.block[0].text);
    let output = run_inspect(&fixture_path(id), true, None);
    assert!(output.status.success());
    let events = parse_events(&output.stdout);
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(
        paragraphs.iter().map(il_paragraph_text).collect::<Vec<_>>(),
        expected,
    );

    let id = "unit-para-07-line-numbers";
    let manifest = fixture_manifest(id);
    let output = run_inspect(&fixture_path(id), true, None);
    let events = parse_events(&output.stdout);
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    let candidates = paragraphs
        .iter()
        .filter(|paragraph| {
            paragraph["text"]["chars"]
                .as_array()
                .unwrap()
                .iter()
                .any(|character| character["layout"]["policy"] == "translate")
        })
        .map(il_paragraph_text)
        .collect::<Vec<_>>();
    assert_eq!(candidates, [manifest.expected.block[4].text.clone()]);
}

#[test]
fn supported_font_and_cmap_fixtures_match_manifest_unicode_and_positive_advances() {
    let directory = tempfile::tempdir().unwrap();
    for id in [
        "unit-font-01-std14-custom-widths",
        "unit-font-escaped-name",
        "unit-stream-02-type3-d1",
        "unit-stream-04-type3-d0",
        "unit-cmap-01-identity-no-tounicode",
        "unit-cmap-02-mixed-codespace",
        "unit-cmap-embedded-ok",
        "unit-cmap-identity-alias",
    ] {
        let manifest = fixture_manifest(id);
        let input = fixture_path(id);
        let inspected = run_inspect(&input, true, None);
        assert!(
            inspected.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        let events = parse_events(&inspected.stdout);
        assert_one_terminal_last(&events, "result");
        let result = events.last().unwrap();

        for page_index in 0..manifest.page.len() {
            let expected = expected_page_text(&manifest, page_index);
            if expected.is_empty() {
                continue;
            }
            let paragraphs = result["il"]["pages"][page_index]["paragraphs"]
                .as_array()
                .unwrap();
            assert!(
                paragraphs
                    .iter()
                    .all(|paragraph| paragraph.get("preserved").is_none()),
                "fixture {id} unexpectedly preserved a processable paragraph"
            );
            let characters = paragraphs
                .iter()
                .flat_map(|paragraph| paragraph["text"]["chars"].as_array().unwrap())
                .collect::<Vec<_>>();
            let actual = characters
                .iter()
                .map(|character| character["unicode"].as_str().unwrap())
                .collect::<String>();
            assert_eq!(actual, expected, "fixture {id}");
            assert_eq!(characters.len(), expected.chars().count(), "fixture {id}");
            for (character_index, character) in characters.into_iter().enumerate() {
                let left = character["box"]["left"].as_f64().unwrap();
                let right = character["box"]["right"].as_f64().unwrap();
                assert!(
                    left.is_finite() && right.is_finite() && right > left,
                    "fixture {id}, character {character_index} has no positive advance box"
                );
            }
        }
        assert!(
            !inspected
                .stdout
                .windows(b"(cid:".len())
                .any(|window| window == b"(cid:"),
            "fixture {id} emitted a CID literal"
        );

        let translated = directory.path().join(format!("{id}.pdf"));
        let translation = run_none(&input, Some(&translated), true);
        assert!(
            translation.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&translation.stderr)
        );
        assert_eq!(
            decoded_page_streams(&translated, 1),
            decoded_page_streams(&input, 1),
            "fixture {id}"
        );
        assert!(
            !std::fs::read(&translated)
                .unwrap()
                .windows(b"(cid:".len())
                .any(|window| window == b"(cid:"),
            "fixture {id} output contains a CID literal"
        );
    }
}

#[test]
fn unreliable_font_and_cmap_fixtures_preserve_exact_bytes_with_declared_reasons() {
    let directory = tempfile::tempdir().unwrap();
    for id in [
        "unit-cmap-predefined-gb",
        "mal-font-missing-resource",
        "mal-font-no-widths",
        "mal-font-truncated-fontfile",
        "mal-font-no-descendant-subtype",
        "mal-font-type3-no-matrix",
        "mal-font-type3-degenerate-matrix",
        "mal-cmap-missing-encoding",
        "mal-cmap-bfrange-arity",
        "mal-cmap-bad-differences",
        "mal-parse-tounicode-not-stream",
    ] {
        let manifest = fixture_manifest(id);
        let expected = manifest
            .expected
            .degradation
            .iter()
            .filter(|degradation| degradation.scope == "paragraph")
            .map(|degradation| {
                serde_json::json!({
                    "page_index": degradation.page,
                    "paragraph_index": degradation.paragraph.unwrap(),
                    "reason": degradation.reason,
                })
            })
            .collect::<Vec<_>>();
        assert!(!expected.is_empty(), "fixture {id}");

        let input = fixture_path(id);
        let inspected = run_inspect(&input, true, None);
        assert!(
            inspected.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        let events = parse_events(&inspected.stdout);
        assert_one_terminal_last(&events, "result");
        let summary = events
            .iter()
            .find(|event| event["event"] == "diagnostic" && event["id"] == "degradation_summary")
            .unwrap_or_else(|| panic!("fixture {id} has no degradation summary"));
        assert_eq!(summary["degraded_page_indices"], serde_json::json!([]));
        assert_eq!(
            summary["preserved_paragraphs"],
            serde_json::json!(expected),
            "fixture {id}"
        );

        let translated = directory.path().join(format!("{id}.pdf"));
        let translation = run_none(&input, Some(&translated), true);
        assert!(
            translation.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&translation.stderr)
        );
        assert_eq!(
            std::fs::read(&translated).unwrap(),
            std::fs::read(&input).unwrap(),
            "fixture {id}"
        );
        assert!(
            !std::fs::read(&translated)
                .unwrap()
                .windows(b"(cid:".len())
                .any(|window| window == b"(cid:"),
            "fixture {id} output contains a CID literal"
        );
    }
}

#[test]
fn mixed_cmap_document_rewrites_seven_pages_and_preserves_three_independently() {
    let id = "intg-cmap-mixed-degrade";
    let manifest = fixture_manifest(id);
    let input = fixture_path(id);
    let inspected = run_inspect(&input, true, None);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let result = events.last().unwrap();

    for page_index in 0..manifest.page.len() {
        let paragraphs = result["il"]["pages"][page_index]["paragraphs"]
            .as_array()
            .unwrap();
        let actual = paragraphs
            .iter()
            .flat_map(|paragraph| paragraph["text"]["chars"].as_array().unwrap())
            .map(|character| character["unicode"].as_str().unwrap())
            .collect::<String>();
        assert_eq!(actual, expected_page_text(&manifest, page_index));
        if page_index < 7 {
            assert!(
                paragraphs
                    .iter()
                    .all(|paragraph| paragraph.get("preserved").is_none()),
                "page {page_index}"
            );
        } else {
            assert_eq!(paragraphs.len(), 1, "page {page_index}");
            assert_eq!(paragraphs[0]["preserved"], "unsupported_font");
        }
    }

    let expected_preserved = manifest
        .expected
        .degradation
        .iter()
        .map(|degradation| {
            serde_json::json!({
                "page_index": degradation.page,
                "paragraph_index": degradation.paragraph.unwrap(),
                "reason": degradation.reason,
            })
        })
        .collect::<Vec<_>>();
    let summary = events
        .iter()
        .find(|event| event["event"] == "diagnostic" && event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["degraded_page_indices"], serde_json::json!([]));
    assert_eq!(
        summary["preserved_paragraphs"],
        serde_json::json!(expected_preserved)
    );

    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("mixed.pdf");
    let translation = run_none(&input, Some(&translated), true);
    assert!(
        translation.status.success(),
        "{}",
        String::from_utf8_lossy(&translation.stderr)
    );
    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&translated).unwrap();
    assert!(output_bytes.starts_with(&input_bytes));
    assert!(output_bytes.len() > input_bytes.len());
    assert!(
        !output_bytes
            .windows(b"(cid:".len())
            .any(|window| window == b"(cid:")
    );

    for page_number in 1..=10 {
        assert_eq!(
            decoded_page_streams(&translated, page_number),
            decoded_page_streams(&input, page_number),
            "page {page_number}"
        );
        if page_number <= 7 {
            assert_ne!(
                page_content_ids(&translated, page_number),
                page_content_ids(&input, page_number),
                "page {page_number} was not rewritten"
            );
        } else {
            assert_eq!(
                page_content_ids(&translated, page_number),
                page_content_ids(&input, page_number),
                "page {page_number} was not preserved"
            );
        }
    }
}

#[test]
fn structured_and_inline_image_programs_round_trip_without_rebuilding_content() {
    let directory = tempfile::tempdir().unwrap();
    for id in [
        "unit-base-03-structured",
        "unit-stream-08-inline-image-EI-in-data",
    ] {
        let input = fixture_path(id);
        let translated = directory.path().join(format!("{id}.pdf"));
        let result = run_none(&input, Some(&translated), false);
        assert!(
            result.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            decoded_page_streams(&translated, 1),
            decoded_page_streams(&input, 1),
            "fixture {id}"
        );
    }
}

#[test]
fn parse_stream_and_xobject_fixture_matrix_stays_bounded_and_preserves_streams() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum OutputExpectation {
        Rewritten,
        Exact,
        Missing,
    }

    let expected = BTreeMap::from([
        (
            "intg-scan-06-blank-middle",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "intg-scan-07-image-middle",
            (0, OutputExpectation::Rewritten, Some("scan_summary")),
        ),
        (
            "unit-base-01-single-line",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "mal-parse-05-contents-array-string-split",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-parse-06-deep-nesting",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-parse-07-parent-cycle",
            (2, OutputExpectation::Missing, None),
        ),
        (
            "mal-parse-08-broken-objstm",
            (2, OutputExpectation::Missing, None),
        ),
        (
            "mal-parse-09-outlines-cycle",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "mal-parse-dangling-annots",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "mal-parse-dangling-critical",
            (2, OutputExpectation::Missing, None),
        ),
        ("mal-parse-null-kid", (2, OutputExpectation::Missing, None)),
        (
            "mal-parse-tounicode-not-stream",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-stream-bad-hex",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-stream-nested-bt",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-stream-03-arity-excess",
            (0, OutputExpectation::Exact, Some("content_recovered")),
        ),
        (
            "mal-stream-04-arity-short",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-stream-05-unbalanced-Q",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-stream-06-glued-tokens",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-stream-07-double-decimal",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-stream-08-unknown-outside-bx",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-stream-09-orphan-text",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-stream-10-unterminated-string",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-stream-11-tj-array-type",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-xobj-01-self-recursive",
            (0, OutputExpectation::Exact, Some("content_recovered")),
        ),
        (
            "mal-xobj-02-mutual-recursive",
            (0, OutputExpectation::Exact, Some("content_recovered")),
        ),
        (
            "mal-xobj-03-form-no-bbox",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-xobj-04-scope-underflow",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-xobj-05-scope-tail",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-xobj-bad-matrix",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-xobj-bbox-null",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-xobj-missing-name",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "unit-parse-01-ascii85",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-02-cascade",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-03-lzw-earlychange",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-03-lzw-earlychange-1",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-04-contents-array-numeric-split",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-05-contents-array-string-parent",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "unit-parse-07-inherited-page-resources",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-11-outline-siblings",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-indirect-filter",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-m1-switchboard",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-parse-midtree-resources",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-00-malformed-parent",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-01-bx-ex-unknown-op",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-02-type3-d1",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-03-unknown-op-outside-bx",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-04-type3-d0",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-08-inline-image-EI-in-data",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-09-inline-image-no-L",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-10-inline-image-length",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-11-inline-image-filtered-fallback",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "unit-stream-odd-hex",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-stream-tr7-clip",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-write-04-xobj-in-objstm",
            (0, OutputExpectation::Exact, None),
        ),
        (
            "unit-xobj-00-recursion-parent",
            (0, OutputExpectation::Exact, None),
        ),
        (
            "unit-xobj-04-inherited-resources",
            (0, OutputExpectation::Exact, None),
        ),
        (
            "unit-xobj-05-scope-parent",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-xobj-05-singular-ctm",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "unit-xobj-depth-overflow",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "unit-xobj-m1-switchboard",
            (0, OutputExpectation::Exact, None),
        ),
    ]);
    let discovered = fixture_ids_with_case_prefixes(&["PARSE-", "STREAM-", "XOBJ-"]);
    assert_eq!(
        expected.keys().copied().collect::<BTreeSet<_>>(),
        discovered
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
    );

    let directory = tempfile::tempdir().unwrap();
    for (id, (exit_code, output_expectation, required_diagnostic)) in expected {
        let manifest = fixture_manifest(id);
        let input = fixture_path(id);
        let inspected = run_inspect(&input, true, None);
        assert_eq!(inspected.status.code(), Some(exit_code), "fixture {id}");
        let events = parse_events(&inspected.stdout);
        assert_one_terminal_last(&events, if exit_code == 0 { "result" } else { "error" });
        if let Some(required) = required_diagnostic {
            assert!(
                events
                    .iter()
                    .any(|event| event["event"] == "diagnostic" && event["id"] == required),
                "fixture {id} has no {required} diagnostic"
            );
        }
        if exit_code == 0 {
            let result = events.last().unwrap();
            for page_index in 0..manifest.page.len() {
                let expected_text = if id == "unit-xobj-depth-overflow" {
                    "MIMUS".to_string()
                } else {
                    expected_page_text(&manifest, page_index)
                };
                if expected_text.is_empty() {
                    continue;
                }
                let paragraphs = result["il"]["pages"][page_index]["paragraphs"]
                    .as_array()
                    .unwrap();
                if paragraphs
                    .iter()
                    .any(|paragraph| paragraph.get("preserved").is_some())
                {
                    continue;
                }
                let actual = paragraphs
                    .iter()
                    .flat_map(|paragraph| paragraph["text"]["chars"].as_array().unwrap())
                    .map(|character| character["unicode"].as_str().unwrap())
                    .collect::<String>();
                assert_eq!(actual, expected_text, "fixture {id}, page {page_index}");
            }
        }

        let translated = directory.path().join(format!("{id}.pdf"));
        let translation = run_none(&input, Some(&translated), true);
        assert_eq!(translation.status.code(), Some(exit_code), "fixture {id}");
        let translation_events = parse_events(&translation.stdout);
        assert_one_terminal_last(
            &translation_events,
            if exit_code == 0 { "result" } else { "error" },
        );
        match output_expectation {
            OutputExpectation::Rewritten => {
                let input_bytes = std::fs::read(&input).unwrap();
                let output_bytes = std::fs::read(&translated).unwrap();
                assert!(output_bytes.starts_with(&input_bytes), "fixture {id}");
                assert!(output_bytes.len() > input_bytes.len(), "fixture {id}");
            }
            OutputExpectation::Exact => assert_eq!(
                std::fs::read(&translated).unwrap(),
                std::fs::read(&input).unwrap(),
                "fixture {id}"
            ),
            OutputExpectation::Missing => {
                assert!(!translated.exists(), "fixture {id} produced output")
            }
        }
        if output_expectation != OutputExpectation::Missing {
            for page_number in 1..=u32::try_from(manifest.page.len()).unwrap() {
                assert_eq!(
                    decoded_page_streams(&translated, page_number),
                    decoded_page_streams(&input, page_number),
                    "fixture {id}, page {page_number}"
                );
            }
        }
    }
}

#[test]
fn doc_04_production_results_match_manifest_transform_and_degradation_expectations() {
    for id in [
        "unit-doc-04-rotated-90",
        "unit-doc-04-rotated-45",
        "unit-doc-04-mirrored",
        "unit-doc-04-skew-15",
        "unit-doc-04-rotate90-compensated",
        "unit-doc-04-mixed-char",
        "mal-doc-04-degenerate-tm",
    ] {
        let manifest = fixture_manifest(id);
        let input = fixture_path(id);
        let inspected = run_inspect(&input, true, None);
        assert!(
            inspected.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        let events = parse_events(&inspected.stdout);
        assert_one_terminal_last(&events, "result");
        let result = events.last().unwrap();

        for page_index in 0..manifest.page.len() {
            let expected = expected_page_transforms(&manifest, page_index);
            if expected.is_empty() {
                continue;
            }
            let actual = result["il"]["pages"][page_index]["paragraphs"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|paragraph| paragraph["text"]["chars"].as_array().unwrap().iter())
                .collect::<Vec<_>>();
            assert_eq!(actual.len(), expected.len(), "fixture {id}");
            for (character_index, (character, (kind, degrees))) in
                actual.into_iter().zip(expected).enumerate()
            {
                assert_eq!(
                    character["text_transform"]["kind"].as_str(),
                    Some(kind.as_str()),
                    "fixture {id}, character {character_index}"
                );
                match degrees {
                    Some(expected) => assert_close(
                        character["text_transform"]["degrees"].as_f64().unwrap(),
                        expected,
                        0.001,
                    ),
                    None => assert!(
                        character["text_transform"].get("degrees").is_none(),
                        "fixture {id}, character {character_index}"
                    ),
                }
            }
        }

        let expected_preserved = manifest
            .expected
            .degradation
            .iter()
            .filter(|expected| expected.scope == "paragraph")
            .map(|expected| {
                serde_json::json!({
                    "page_index": expected.page,
                    "paragraph_index": expected.paragraph.unwrap(),
                    "reason": expected.reason,
                })
            })
            .collect::<Vec<_>>();
        let summary = events
            .iter()
            .find(|event| event["event"] == "diagnostic" && event["id"] == "degradation_summary");
        if expected_preserved.is_empty() {
            assert!(summary.is_none(), "fixture {id}");
        } else {
            assert_eq!(
                summary.unwrap()["preserved_paragraphs"],
                serde_json::json!(expected_preserved),
                "fixture {id}"
            );
        }

        let directory = tempfile::tempdir().unwrap();
        let translated = directory.path().join(format!("{id}-translated.pdf"));
        let translated_result = run_none(&input, Some(&translated), false);
        assert!(
            translated_result.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&translated_result.stderr)
        );
        if manifest.expected.degradation.is_empty() {
            assert_eq!(
                decoded_page_streams(&translated, 1),
                decoded_page_streams(&input, 1),
                "fixture {id}"
            );
        } else {
            assert_eq!(
                std::fs::read(&translated).unwrap(),
                std::fs::read(&input).unwrap(),
                "fixture {id}"
            );
        }
    }
}

#[test]
fn geometry_fixtures_match_manifest_frames_and_preserve_page_box_entries() {
    for id in [
        "unit-geom-06-mediabox-double-space",
        "unit-geom-06-mediabox-indirect",
        "unit-geom-08-cropbox-inherited",
    ] {
        let manifest = fixture_manifest(id);
        let input = fixture_path(id);
        let inspected = run_inspect(&input, true, None);
        assert!(
            inspected.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        let events = parse_events(&inspected.stdout);
        assert_one_terminal_last(&events, "result");
        assert!(
            events.iter().all(|event| event["id"] != "page_degraded"),
            "fixture {id}"
        );
        let page = &manifest.page[0];
        let effective_box = page.effective_box().unwrap();
        let mut expected_width = effective_box[2] - effective_box[0];
        let mut expected_height = effective_box[3] - effective_box[1];
        if page.rotate.rem_euclid(180) != 0 {
            std::mem::swap(&mut expected_width, &mut expected_height);
        }
        let geometry = &events.last().unwrap()["il"]["pages"][0]["geometry"];
        assert_close(geometry["width"].as_f64().unwrap(), expected_width, 0.001);
        assert_close(geometry["height"].as_f64().unwrap(), expected_height, 0.001);
        assert_eq!(geometry["rotate_degrees"], page.rotate);

        let directory = tempfile::tempdir().unwrap();
        let translated = directory.path().join(format!("{id}-translated.pdf"));
        let translated_result = run_none(&input, Some(&translated), false);
        assert!(
            translated_result.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&translated_result.stderr)
        );
        assert_eq!(
            decoded_page_streams(&translated, 1),
            decoded_page_streams(&input, 1),
            "fixture {id}"
        );
        for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate"] {
            assert_eq!(
                local_page_entry(&translated, 1, key),
                local_page_entry(&input, 1, key),
                "fixture {id}, key {}",
                String::from_utf8_lossy(key)
            );
        }
    }
}

#[test]
fn malformed_geometry_fixtures_degrade_the_declared_page_without_rewriting_it() {
    for id in ["mal-geom-07-mediabox-null", "mal-geom-02-rotate-45"] {
        let manifest = fixture_manifest(id);
        let expected = manifest
            .expected
            .degradation
            .iter()
            .find(|expected| expected.scope == "page")
            .unwrap();
        assert!(expected.paragraph.is_none());
        let input = fixture_path(id);
        let directory = tempfile::tempdir().unwrap();
        let translated = directory.path().join(format!("{id}-translated.pdf"));
        let output = run_none(&input, Some(&translated), true);
        assert!(
            output.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = parse_events(&output.stdout);
        assert_one_terminal_last(&events, "result");
        let degraded = events
            .iter()
            .find(|event| event["event"] == "diagnostic" && event["id"] == "page_degraded")
            .unwrap();
        assert_eq!(degraded["page_index"], expected.page, "fixture {id}");
        assert_eq!(degraded["reason"], expected.reason, "fixture {id}");
        let summary = events
            .iter()
            .find(|event| event["event"] == "diagnostic" && event["id"] == "degradation_summary")
            .unwrap();
        assert_eq!(
            summary["degraded_page_indices"],
            serde_json::json!([expected.page]),
            "fixture {id}"
        );
        assert_eq!(
            std::fs::read(&translated).unwrap(),
            std::fs::read(&input).unwrap(),
            "fixture {id}"
        );
    }
}

/// ADR-0013 §6：合法 `/Rotate` 进入视觉页框朝向分类，而不是作为页级降级处理。
#[test]
fn a_legal_rotated_page_uses_the_visual_transform_without_degradation() {
    let directory = tempfile::tempdir().unwrap();
    let input = write_rotated_pdf(directory.path(), "rotate-90.pdf", 90);
    let translated = directory.path().join("rotated.pdf");
    let inspected = run_inspect(&input, true, None);
    assert!(inspected.status.success());
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let chars = events.last().unwrap()["il"]["pages"][0]["paragraphs"][0]["text"]["chars"]
        .as_array()
        .unwrap();
    assert!(chars.iter().all(|character| {
        character["text_transform"]
            == serde_json::json!({
                "kind": "rotated",
                "degrees": 90.0,
            })
    }));

    let result = run_none(&input, Some(&translated), false);

    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!stderr.contains("warning[page_degraded]"), "{stderr}");
    assert!(!stderr.contains("warning[degradation_summary]"), "{stderr}");
    assert_eq!(
        std::fs::read(&translated).unwrap(),
        std::fs::read(&input).unwrap(),
        "an all-non-upright page has no replacement spans"
    );
}

/// `mal-stream-10-unterminated-string` 的 STREAM-08-page-degrades 与
/// STREAM-08-no-partial-il 两条声明行为，在生产路径上的对应断言。
#[test]
fn a_truncated_content_stream_degrades_its_page() {
    let directory = tempfile::tempdir().unwrap();
    let input = fixture_path("mal-stream-10-unterminated-string");
    let translated = directory.path().join("truncated.pdf");
    let result = run_none(&input, Some(&translated), false);

    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("warning[page_degraded]: page 1 kept as-is (content stream syntax"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read(&translated).unwrap(),
        std::fs::read(&input).unwrap(),
        "a page whose tokenizer ran off the end must be republished byte for byte"
    );
}

#[test]
fn strict_mode_turns_page_degradation_into_translation_exit_without_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let input = fixture_path("mal-stream-10-unterminated-string");
    let output_path = directory.path().join("strict.pdf");
    std::fs::write(&output_path, b"existing destination").unwrap();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let output = command
        .env(PDFIUM_ENV, pdfium_library())
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--strict",
            "--layout",
            "single-line",
            "--output",
        ])
        .arg(&output_path)
        .arg(&input)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stderr.is_empty());
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "error");
    let resolved = events
        .iter()
        .find(|event| event["event"] == "configuration_resolved")
        .unwrap();
    assert_eq!(resolved["strict"], true);
    assert!(events.iter().any(|event| {
        event["event"] == "diagnostic"
            && event["id"] == "page_degraded"
            && event["reason"] == "content_stream_syntax"
    }));
    assert!(events.iter().any(|event| {
        event["event"] == "diagnostic"
            && event["id"] == "degradation_summary"
            && event["degraded_page_indices"] == serde_json::json!([0])
    }));
    assert_eq!(events.last().unwrap()["category"], "translation");
    assert_eq!(events.last().unwrap()["reason"], "strict_degradation");
    assert_eq!(std::fs::read(output_path).unwrap(), b"existing destination");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

    let human_path = directory.path().join("strict-human.pdf");
    std::fs::write(&human_path, b"human destination").unwrap();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let human = command
        .env(PDFIUM_ENV, pdfium_library())
        .args([
            "translate",
            "--backend",
            "none",
            "--strict",
            "--layout",
            "single-line",
            "--output",
        ])
        .arg(&human_path)
        .arg(input)
        .output()
        .unwrap();
    assert_eq!(human.status.code(), Some(4));
    assert!(human.stdout.is_empty());
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(stderr.contains("warning[page_degraded]"));
    assert!(stderr.contains("warning[degradation_summary]"));
    assert!(stderr.contains("error[strict_degradation]"));
    assert_eq!(std::fs::read(human_path).unwrap(), b"human destination");
}

/// `mal-stream-09-orphan-text` 与 `mal-stream-11-tj-array-type` 的声明行为在生产
/// 路径上的对应断言：文字一个不少地进入 IL，恢复每页只报一次。
#[test]
fn malformed_content_streams_are_recovered_and_reported_once_per_page() {
    for (id, recovery) in [
        ("mal-stream-09-orphan-text", "text operators outside BT/ET"),
        (
            "mal-stream-11-tj-array-type",
            "an illegal element inside a TJ array",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let input = fixture_path(id);
        let translated = directory.path().join("recovered.pdf");
        let result = run_none(&input, Some(&translated), false);
        assert_eq!(
            result.status.code(),
            Some(0),
            "fixture {id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );

        let stderr = String::from_utf8_lossy(&result.stderr);
        let warnings = stderr
            .lines()
            .filter(|line| line.starts_with("warning[content_recovered]:"))
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1, "fixture {id}: {stderr}");
        assert!(warnings[0].contains(recovery), "fixture {id}: {stderr}");
        assert!(
            !stderr.contains("warning[page_degraded]"),
            "fixture {id} must be translated, not degraded: {stderr}"
        );

        let il: serde_json::Value =
            serde_json::from_slice(&run_inspect(&input, false, None).stdout).unwrap();
        let text = il["pages"][0]["paragraphs"][0]["text"]["chars"]
            .as_array()
            .unwrap_or_else(|| panic!("fixture {id} produced no characters"))
            .iter()
            .filter_map(|character| character["unicode"].as_str())
            .collect::<String>();
        assert_eq!(text, "MIMUS", "fixture {id} lost characters");
    }
}

#[test]
fn graphics_text_state_and_multiple_lines_round_trip_without_content_loss() {
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
        assert!(
            result.status.success(),
            "input {name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            decoded_page_streams(&translated, 1),
            decoded_page_streams(&input, 1),
            "input {name}"
        );
    }
}

#[test]
fn missing_pdfium_uses_asset_exit_code_three() {
    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("missing-pdfium.pdf");
    let result = Command::new(BIN)
        .env(PDFIUM_ENV, directory.path().join("missing-libpdfium.dylib"))
        .env("MIMUS_FONT_REGULAR", test_font_path("Regular"))
        .env("MIMUS_FONT_BOLD", test_font_path("Bold"))
        .env(
            "MIMUS_FONT_FALLBACK_REGULAR",
            test_fallback_font_path("Regular"),
        )
        .env("MIMUS_FONT_FALLBACK_BOLD", test_fallback_font_path("Bold"))
        .args([
            "translate",
            "--backend",
            "none",
            "--layout",
            "single-line",
            "--output",
        ])
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
        .env("MIMUS_FONT_REGULAR", test_font_path("Regular"))
        .env("MIMUS_FONT_BOLD", test_font_path("Bold"))
        .env(
            "MIMUS_FONT_FALLBACK_REGULAR",
            test_fallback_font_path("Regular"),
        )
        .env("MIMUS_FONT_FALLBACK_BOLD", test_fallback_font_path("Bold"))
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--layout",
            "single-line",
            "--output",
        ])
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

#[test]
fn writeback_fixture_matrix_preserves_prefix_structure_and_resource_identity() {
    for id in [
        "unit-write-01-bookmarks-rich",
        "unit-write-02-shared-resources",
        "unit-write-03-resources-gen-nonzero",
        "unit-write-04-xobj-in-objstm",
        "unit-write-05-indirect-resources-objstm",
        "unit-write-06-free-object-slot",
    ] {
        let input = fixture_path(id);
        let directory = tempfile::tempdir().unwrap();
        let translated = directory.path().join(format!("{id}.pdf"));
        let result = run_none(&input, Some(&translated), false);
        assert!(
            result.status.success(),
            "{id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let input_bytes = std::fs::read(&input).unwrap();
        let output_bytes = std::fs::read(&translated).unwrap();
        assert!(output_bytes.starts_with(&input_bytes), "{id}");
        let qpdf = Command::new("qpdf")
            .arg("--check")
            .arg(&translated)
            .output()
            .unwrap();
        assert!(
            qpdf.status.success(),
            "{id}: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        let poppler = Command::new("pdftotext")
            .arg(&translated)
            .arg("-")
            .output()
            .unwrap();
        let mupdf = Command::new("mutool")
            .args(["draw", "-F", "txt"])
            .arg(&translated)
            .output()
            .unwrap();
        assert!(poppler.status.success(), "{id}");
        assert!(mupdf.status.success(), "{id}");
        assert!(!poppler.stdout.is_empty(), "{id}");
        assert!(!mupdf.stdout.is_empty(), "{id}");
    }

    let input = fixture_path("unit-write-01-bookmarks-rich");
    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("rich.pdf");
    assert!(run_none(&input, Some(&translated), false).status.success());
    for object in [1, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17] {
        assert_eq!(
            qpdf_object(&translated, &object.to_string()),
            qpdf_object(&input, &object.to_string()),
            "rich structure object {object} changed"
        );
    }

    let input = fixture_path("unit-write-03-resources-gen-nonzero");
    let translated = directory.path().join("generation.pdf");
    assert!(run_none(&input, Some(&translated), false).status.success());
    assert!(
        String::from_utf8(qpdf_object(&translated, "3"))
            .unwrap()
            .contains("/Resources 4 7 R")
    );

    let input = fixture_path("unit-write-06-free-object-slot");
    let translated = directory.path().join("free-slot.pdf");
    assert!(run_none(&input, Some(&translated), false).status.success());
    assert!(
        page_content_ids(&translated, 1)
            .iter()
            .all(|(object, _)| *object > 10)
    );
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

fn qpdf_object(pdf: &Path, object: &str) -> Vec<u8> {
    let output = Command::new("qpdf")
        .arg(format!("--show-object={object}"))
        .arg(pdf)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
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
