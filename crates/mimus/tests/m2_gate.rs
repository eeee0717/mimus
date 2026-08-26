use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

const BIN: &str = env!("CARGO_BIN_EXE_mimus");
const PDFIUM_ENV: &str = "MIMUS_PDFIUM_LIBRARY";
const SECRET_CANARY: &str = "mimus-m2-secret-canary";

#[derive(Debug, Deserialize)]
struct FixtureManifest {
    identity: ManifestIdentity,
    expected: ManifestExpected,
}

#[derive(Debug, Deserialize)]
struct ManifestIdentity {
    legality: String,
    cases: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestExpected {
    renderer_diagnostic: Option<String>,
}

#[derive(Debug, Clone)]
struct RecordedRequest {
    path: String,
    model: String,
    input: String,
    kind: RequestKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    TermExtraction,
    ParagraphTranslation,
}

#[derive(Clone)]
enum ScriptedReply {
    Status(&'static str),
    Output(&'static str),
    Echo,
    Body(&'static str),
    DelayedOutput(Duration, &'static str),
    Disconnect,
}

struct GateResponsesServer {
    endpoint: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    violations: Arc<Mutex<Vec<String>>>,
    replies: Arc<Mutex<VecDeque<ScriptedReply>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl GateResponsesServer {
    fn start(replies: impl IntoIterator<Item = ScriptedReply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);
        let violations = Arc::new(Mutex::new(Vec::new()));
        let captured_violations = Arc::clone(&violations);
        let replies = Arc::new(Mutex::new(replies.into_iter().collect()));
        let scripted_replies = Arc::clone(&replies);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let requests = Arc::clone(&captured_requests);
                        let violations = Arc::clone(&captured_violations);
                        let replies = Arc::clone(&scripted_replies);
                        handlers.push(thread::spawn(move || {
                            handle_request(&mut stream, &requests, &violations, &replies);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("M2 fake Responses server accept failed: {error}"),
                }
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        Self {
            endpoint: format!("http://{address}"),
            requests,
            violations,
            replies,
            stop,
            thread: Some(thread),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn request_count_by_kind(&self, kind: RequestKind) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.kind == kind)
            .count()
    }

    fn assert_clean(&self) {
        assert!(
            self.violations.lock().unwrap().is_empty(),
            "fake Responses server observed a wire-contract violation"
        );
        assert!(
            self.replies.lock().unwrap().is_empty(),
            "not every scripted fake response was consumed"
        );
    }
}

impl Drop for GateResponsesServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.endpoint.trim_start_matches("http://"));
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn handle_request(
    stream: &mut TcpStream,
    requests: &Mutex<Vec<RecordedRequest>>,
    violations: &Mutex<Vec<String>>,
    replies: &Mutex<VecDeque<ScriptedReply>>,
) {
    let request = read_http_request(stream);
    if request.is_empty() {
        return;
    }
    let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
        violations
            .lock()
            .unwrap()
            .push("missing headers".to_owned());
        return;
    };
    let header_bytes = &request[..header_end];
    let headers = String::from_utf8_lossy(header_bytes);
    let request_line = headers.lines().next().unwrap_or_default();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    if !request_line.starts_with("POST ") || path != "/v1/responses" {
        violations
            .lock()
            .unwrap()
            .push("request did not use POST /v1/responses".to_owned());
    }
    let authorization_is_valid = headers.lines().any(|line| {
        line.strip_prefix("Authorization: ")
            .or_else(|| line.strip_prefix("authorization: "))
            == Some(&format!("Bearer {SECRET_CANARY}"))
    });
    if !authorization_is_valid {
        violations
            .lock()
            .unwrap()
            .push("authorization header was absent or invalid".to_owned());
    }
    let payload: serde_json::Value = match serde_json::from_slice(&request[header_end + 4..]) {
        Ok(value) => value,
        Err(_) => {
            violations
                .lock()
                .unwrap()
                .push("request body was not JSON".to_owned());
            return;
        }
    };
    if payload.to_string().contains(SECRET_CANARY) {
        violations
            .lock()
            .unwrap()
            .push("request body contained the API key".to_owned());
    }
    let model = payload["model"].as_str().unwrap_or_default().to_owned();
    let input = payload["input"].as_str().unwrap_or_default().to_owned();
    let instructions = payload["instructions"].as_str().unwrap_or_default();
    if model.is_empty() || input.is_empty() || instructions.is_empty() {
        violations
            .lock()
            .unwrap()
            .push("Responses request omitted model, instructions, or input".to_owned());
    }
    let kind = if instructions.contains("Extract important technical terms") {
        RequestKind::TermExtraction
    } else {
        RequestKind::ParagraphTranslation
    };
    requests.lock().unwrap().push(RecordedRequest {
        path,
        model,
        input: input.clone(),
        kind,
    });

    let reply = if kind == RequestKind::TermExtraction {
        Some(ScriptedReply::Output(r#"{"terms":[]}"#))
    } else {
        replies.lock().unwrap().pop_front()
    };
    let (status, body) = match reply {
        Some(ScriptedReply::Status(status)) => {
            (status, r#"{"error":"scripted transient"}"#.to_owned())
        }
        Some(ScriptedReply::Output(output)) => (
            "200 OK",
            serde_json::json!({ "output_text": output }).to_string(),
        ),
        Some(ScriptedReply::Echo) => (
            "200 OK",
            serde_json::json!({ "output_text": input }).to_string(),
        ),
        Some(ScriptedReply::Body(body)) => ("200 OK", body.to_owned()),
        Some(ScriptedReply::DelayedOutput(delay, output)) => {
            thread::sleep(delay);
            (
                "200 OK",
                serde_json::json!({ "output_text": output }).to_string(),
            )
        }
        Some(ScriptedReply::Disconnect) => return,
        None => (
            "200 OK",
            serde_json::json!({ "output_text": deterministic_translation(&input) }).to_string(),
        ),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn deterministic_translation(input: &str) -> String {
    const HAN_SAMPLE: &str = "模型数据证论文翻译语结果保持结构程稳定缓存重试诊断排版字体";

    let sample = HAN_SAMPLE.chars().collect::<Vec<_>>();
    let seed = input.bytes().fold(2_166_136_261_usize, |hash, byte| {
        (hash ^ usize::from(byte)).wrapping_mul(16_777_619)
    });
    let mut output = String::new();
    let mut rest = input;
    let mut emitted_text = false;
    let mut segment_index = 0_usize;
    while !rest.is_empty() {
        if (rest.starts_with("<b") || rest.starts_with("</b"))
            && let Some(end) = rest.find('>')
        {
            let marker = &rest[..=end];
            let index = marker
                .strip_prefix("<b")
                .and_then(|value| value.strip_suffix('>'))
                .or_else(|| {
                    marker
                        .strip_prefix("</b")
                        .and_then(|value| value.strip_suffix('>'))
                });
            if index.is_some_and(|value| value.parse::<usize>().is_ok_and(|value| value > 0)) {
                output.push_str(marker);
                rest = &rest[marker.len()..];
                continue;
            }
        }
        if (rest.starts_with("{v") || rest.starts_with("{l"))
            && let Some(end) = rest.find('}')
        {
            let marker = &rest[..=end];
            if marker[2..marker.len() - 1]
                .parse::<usize>()
                .is_ok_and(|value| value > 0)
            {
                output.push_str(marker);
                rest = &rest[marker.len()..];
                continue;
            }
        }
        let next_marker = [
            rest.find("<b"),
            rest.find("</b"),
            rest.find("{v"),
            rest.find("{l"),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
        let segment = &rest[..next_marker.max(1)];
        let source_characters = segment
            .chars()
            .filter(|character| !character.is_whitespace())
            .count();
        if source_characters > 0 {
            let output_characters = (source_characters / 2).clamp(1, 6);
            for offset in 0..output_characters {
                output.push(sample[(seed + segment_index * 7 + offset * 5) % sample.len()]);
            }
            emitted_text = true;
            segment_index += 1;
        }
        rest = &rest[segment.len()..];
    }
    if !emitted_text && output.is_empty() {
        output.push(sample[seed % sample.len()]);
    }
    output
}

#[test]
fn deterministic_fake_translation_is_varied_and_preserves_indexed_protocol_markers() {
    let input = "Alpha <b1>bold</b1> {v2} literal {l3}v1}";
    let translated = deterministic_translation(input);

    assert_eq!(translated, deterministic_translation(input));
    assert_ne!(translated, deterministic_translation("Beta"));
    assert!(translated.contains("<b1>"));
    assert!(translated.contains("</b1>"));
    assert!(translated.contains("{v2}"));
    assert!(translated.contains("{l3}"));
    let han = translated
        .chars()
        .filter(|character| ('\u{4e00}'..='\u{9fff}').contains(character))
        .collect::<BTreeSet<_>>();
    assert!(
        han.len() >= 4,
        "fake translation was not varied: {translated}"
    );
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

fn pdfium_library() -> OsString {
    let path = std::env::var_os(PDFIUM_ENV)
        .expect("MIMUS_PDFIUM_LIBRARY must point to the pinned test library");
    assert!(Path::new(&path).is_file(), "PDFium test library is missing");
    path
}

fn test_font_path(weight: &str) -> PathBuf {
    repo_root()
        .join("crates/mimus/tests/assets/fonts")
        .join(format!("MimusTestGB2312-{weight}.ttf"))
}

fn fixture_manifest(id: &str) -> FixtureManifest {
    let path = repo_root()
        .join("corpus/fixtures")
        .join(id)
        .join("manifest.toml");
    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn all_fixture_ids() -> BTreeSet<String> {
    std::fs::read_dir(repo_root().join("corpus/fixtures"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("manifest.toml").is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect()
}

struct RunOptions<'a> {
    output: &'a Path,
    debug: Option<&'a Path>,
    cache: Option<&'a Path>,
    model: &'a str,
    target_language: &'a str,
    glossary: Option<&'a Path>,
    auto_terms: bool,
    strict: bool,
}

fn run_openai(id: &str, server: &GateResponsesServer, options: RunOptions<'_>) -> Output {
    let input = repo_root()
        .join("corpus/fixtures")
        .join(id)
        .join(format!("{id}.pdf"));
    let recording = repo_root()
        .join("corpus/layout-recordings")
        .join(format!("{id}.json"));
    let recording = recording.is_file().then_some(recording);
    run_openai_path(&input, recording.as_deref(), server, options)
}

fn run_openai_path(
    input: &Path,
    layout_recording: Option<&Path>,
    server: &GateResponsesServer,
    options: RunOptions<'_>,
) -> Output {
    let config_file = options.output.parent().unwrap().join("absent-config.toml");
    let mut command = Command::new(BIN);
    command
        .env(PDFIUM_ENV, pdfium_library())
        .env("MIMUS_FONT_REGULAR", test_font_path("Regular"))
        .env("MIMUS_FONT_BOLD", test_font_path("Bold"))
        .env("MIMUS_OPENAI_API_KEY", SECRET_CANARY)
        .env("MIMUS_CONFIG_FILE", config_file)
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env_remove("OPENAI_API_KEY")
        .env_remove("API_KEY")
        .args([
            "--json",
            "translate",
            "--backend",
            "openai",
            "--endpoint",
            &server.endpoint,
            "--model",
            options.model,
            "--target-language",
            options.target_language,
            "--output",
        ])
        .arg(options.output);
    if let Some(cache) = options.cache {
        command.arg("--cache").arg(cache);
    } else {
        command.arg("--no-cache");
    }
    if !options.auto_terms {
        command.arg("--no-auto-terms");
    }
    if options.strict {
        command.arg("--strict");
    }
    if let Some(debug) = options.debug {
        command.arg("--debug").arg(debug);
    }
    if let Some(glossary) = options.glossary {
        command.arg("--glossary").arg(glossary);
    }
    if let Some(recording) = layout_recording {
        command.arg("--layout-replay").arg(recording);
    }
    command.arg(input).output().unwrap()
}

fn parse_events(output: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(output.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn assert_terminal(events: &[serde_json::Value], terminal: &str) {
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["schema_version"] == 2));
    assert_eq!(events.last().unwrap()["event"], terminal);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event["event"].as_str(), Some("result" | "error")))
            .count(),
        1
    );
}

fn assert_secret_absent(bytes: &[u8], source: &str) {
    assert!(
        !bytes
            .windows(SECRET_CANARY.len())
            .any(|window| window == SECRET_CANARY.as_bytes()),
        "API key leaked through {source}"
    );
}

fn assert_tree_has_no_secret(root: &Path) {
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                std::fs::read_dir(&path)
                    .unwrap()
                    .filter_map(Result::ok)
                    .map(|entry| entry.path()),
            );
        } else if path.is_file() {
            assert_secret_absent(&std::fs::read(&path).unwrap(), &path.display().to_string());
        }
    }
}

fn assert_valid_pdf(path: &Path, id: &str) {
    let qpdf = Command::new("qpdf")
        .arg("--check")
        .arg(path)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "fixture {id}: qpdf rejected output: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let poppler = Command::new("pdftotext")
        .arg(path)
        .arg("-")
        .output()
        .unwrap();
    assert!(
        poppler.status.success(),
        "fixture {id}: Poppler rejected output: {}",
        String::from_utf8_lossy(&poppler.stderr)
    );

    let manifest = fixture_manifest(id);
    let mupdf = if manifest.expected.renderer_diagnostic.is_some() {
        Command::new("mutool")
            .arg("info")
            .arg(path)
            .output()
            .unwrap()
    } else {
        Command::new("mutool")
            .args(["draw", "-q", "-F", "txt", "-o", "/dev/null"])
            .arg(path)
            .output()
            .unwrap()
    };
    assert!(
        mupdf.status.success(),
        "fixture {id}: MuPDF rejected output: {}",
        String::from_utf8_lossy(&mupdf.stderr)
    );
}

fn extract_pdf_text(path: &Path, extractor: &str) -> String {
    let output = match extractor {
        "poppler" => Command::new("pdftotext")
            .arg(path)
            .arg("-")
            .output()
            .unwrap(),
        "mupdf" => Command::new("mutool")
            .args(["draw", "-q", "-F", "txt", "-o", "-"])
            .arg(path)
            .output()
            .unwrap(),
        _ => panic!("unknown PDF extractor {extractor}"),
    };
    assert!(
        output.status.success(),
        "{extractor} failed to extract {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn decoded_page_streams(path: &Path, page_number: u32) -> Vec<Vec<u8>> {
    let document = lopdf::Document::load(path).unwrap();
    let page = document.get_pages()[&page_number];
    document
        .get_page_contents(page)
        .into_iter()
        .map(|object| {
            document
                .get_object(object)
                .unwrap()
                .as_stream()
                .unwrap()
                .decompressed_content()
                .unwrap()
        })
        .collect()
}

fn translated_han_strings(snapshot: &Path) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(snapshot).unwrap()).unwrap();
    value["pages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page["paragraphs"].as_array().unwrap())
        .filter_map(|paragraph| paragraph["translated_text"].as_str())
        .filter(|text| {
            text.chars()
                .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character))
        })
        .map(str::to_owned)
        .collect()
}

fn write_repeated_lines_pdf(directory: &Path, line_count: usize) -> PathBuf {
    let input =
        repo_root().join("corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf");
    let output = directory.join("diagnostic-flood.pdf");
    let mut document = lopdf::Document::load(input).unwrap();
    document
        .get_object_mut((3, 0))
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set(
            "MediaBox",
            lopdf::Object::Array(vec![0.into(), 0.into(), 300.into(), 1000.into()]),
        );
    let mut program = String::from("BT /F1 8 Tf\n");
    for index in 0..line_count {
        let baseline = 850_i64 - i64::try_from(index).unwrap() * 11;
        program.push_str(&format!("1 0 0 1 72 {baseline} Tm (MIMUS) Tj\n"));
    }
    program.push_str("ET\n");
    document
        .get_object_mut((9, 0))
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .set_plain_content(program.into_bytes());
    document.save(&output).unwrap();
    output
}

fn write_repeated_lines_layout(directory: &Path, line_count: usize) -> PathBuf {
    let regions = (0..line_count)
        .map(|index| {
            let baseline = 850.0 - index as f64 * 11.0;
            serde_json::json!({
                "bounds": {
                    "left": 68.0,
                    "bottom": baseline - 4.0,
                    "right": 118.0,
                    "top": baseline + 6.0
                },
                "reading_order": index,
                "label": "text",
                "source": "model",
                "confidence": 1.0
            })
        })
        .collect::<Vec<_>>();
    let path = directory.join("diagnostic-flood-layout.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "pages": [{
                "page_index": 0,
                "geometry": {
                    "width": 300.0,
                    "height": 1000.0,
                    "rotate_degrees": 0
                },
                "regions": regions
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn write_relative_tail_pdf(directory: &Path) -> PathBuf {
    let input =
        repo_root().join("corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf");
    let output = directory.join("relative-tail.pdf");
    let mut document = lopdf::Document::load(input).unwrap();
    document
        .get_object_mut((9, 0))
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .set_plain_content(
            b"BT /F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\n0 -20 Td (TAIL) Tj\nET\n".to_vec(),
        );
    document.save(&output).unwrap();
    output
}

fn write_relative_tail_layout(directory: &Path) -> PathBuf {
    let path = directory.join("relative-tail-layout.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "pages": [{
                "page_index": 0,
                "geometry": {
                    "width": 300.0,
                    "height": 200.0,
                    "rotate_degrees": 0
                },
                "regions": [
                    {
                        "bounds": {
                            "left": 68.0,
                            "bottom": 114.0,
                            "right": 116.0,
                            "top": 132.0
                        },
                        "reading_order": 0,
                        "label": "text",
                        "source": "model",
                        "confidence": 1.0
                    },
                    {
                        "bounds": {
                            "left": 68.0,
                            "bottom": 94.0,
                            "right": 112.0,
                            "top": 112.0
                        },
                        "reading_order": 1,
                        "label": "number",
                        "source": "model",
                        "confidence": 1.0
                    }
                ]
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn write_nested_form_with_bad_matrix(directory: &Path) -> PathBuf {
    let input =
        repo_root().join("corpus/fixtures/unit-xobj-depth-overflow/unit-xobj-depth-overflow.pdf");
    let output = directory.join("nested-form-bad-matrix.pdf");
    let mut document = lopdf::Document::load(input).unwrap();
    document
        .get_object_mut((40, 0))
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .dict
        .set(
            "Matrix",
            lopdf::Object::Array(vec![1.into(), 0.into(), 0.into(), 1.into()]),
        );
    document.save(&output).unwrap();
    output
}

fn assert_single_preserved_paragraph(output: &Output, reason: &str) {
    assert!(output.status.success());
    let events = parse_events(&output.stdout);
    assert_terminal(&events, "result");
    let summary = events
        .iter()
        .find(|event| event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["preserved_paragraph_count"], 1);
    assert_eq!(summary["preserved_paragraphs"][0]["reason"], reason);
}

#[test]
fn realistic_han_survives_every_il_stage_and_both_pdf_extractors() {
    const EXPECTED_HAN: [&str; 2] = ["模论结构存排", "稳试字据译持"];

    let directory = tempfile::tempdir().unwrap();
    let debug = directory.path().join("debug");
    let output_path = directory.path().join("translated.pdf");
    let server = GateResponsesServer::start([]);
    let output = run_openai(
        "unit-layout-07-policy-zones",
        &server,
        RunOptions {
            output: &output_path,
            debug: Some(&debug),
            cache: None,
            model: "m2-han-conservation-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: false,
        },
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    for stage in ["06-translate", "07-typeset", "08-font_embed", "09-write"] {
        let strings = translated_han_strings(&debug.join(format!("{stage}.il.json")));
        assert_eq!(
            strings,
            EXPECTED_HAN,
            "Han changed at {stage}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    for extractor in ["poppler", "mupdf"] {
        let extracted = extract_pdf_text(&output_path, extractor);
        for expected in EXPECTED_HAN {
            assert!(
                extracted.contains(expected),
                "{extractor} lost {expected}: {extracted:?}"
            );
        }
    }
    assert_valid_pdf(&output_path, "unit-layout-07-policy-zones");
    server.assert_clean();
}

#[test]
fn translated_text_does_not_move_a_relative_passthrough_line() {
    let directory = tempfile::tempdir().unwrap();
    let input = write_relative_tail_pdf(directory.path());
    let recording = write_relative_tail_layout(directory.path());
    let output_path = directory.path().join("relative-tail.zh.pdf");
    let server = GateResponsesServer::start([ScriptedReply::Output("中文")]);
    let output = run_openai_path(
        &input,
        Some(&recording),
        &server,
        RunOptions {
            output: &output_path,
            debug: None,
            cache: None,
            model: "m2-relative-tail-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: false,
        },
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    for extractor in ["poppler", "mupdf"] {
        let extracted = extract_pdf_text(&output_path, extractor);
        assert!(extracted.contains("中文"), "{extractor}: {extracted:?}");
        assert!(extracted.contains("TAIL"), "{extractor}: {extracted:?}");
    }
    server.assert_clean();
}

#[test]
fn output_font_coverage_miss_degrades_only_the_affected_paragraph() {
    let directory = tempfile::tempdir().unwrap();
    let debug = directory.path().join("debug");
    let output_path = directory.path().join("coverage-miss.pdf");
    let server = GateResponsesServer::start([ScriptedReply::Output("龘")]);
    let output = run_openai(
        "unit-layout-07-policy-zones",
        &server,
        RunOptions {
            output: &output_path,
            debug: Some(&debug),
            cache: None,
            model: "m2-coverage-miss-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: false,
        },
    );

    assert!(output.status.success());
    let events = parse_events(&output.stdout);
    let summary = events
        .iter()
        .find(|event| event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["preserved_paragraph_count"], 1);
    assert_eq!(
        summary["preserved_paragraphs"][0]["reason"],
        "unsupported_font"
    );
    let diagnostic = events
        .iter()
        .find(|event| event["id"] == "unsupported_output_glyph")
        .unwrap();
    assert_eq!(diagnostic["missing_characters"], "龘");
    assert!(
        diagnostic["font_source"]
            .as_str()
            .unwrap()
            .contains("MimusTestGB2312-Regular.ttf")
    );
    assert_eq!(
        diagnostic["font_sha256"],
        "510d0470ca8b77f035fe8e7143526207088c1bdad017451cf253020f72397d63"
    );

    let surviving = translated_han_strings(&debug.join("09-write.il.json"));
    assert_eq!(surviving.len(), 1, "unaffected paragraph did not survive");
    assert!(!surviving[0].contains('龘'));
    for extractor in ["poppler", "mupdf"] {
        let extracted = extract_pdf_text(&output_path, extractor);
        assert!(
            extracted.contains(&surviving[0]),
            "{extractor}: {extracted:?}"
        );
        assert!(!extracted.contains('龘'), "{extractor}: {extracted:?}");
    }
    server.assert_clean();
}

#[test]
fn echo_is_identity_in_strict_mode_and_is_reused_from_cache() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("identities.redb");
    let server = GateResponsesServer::start([ScriptedReply::Echo, ScriptedReply::Echo]);
    let first = run_openai(
        "unit-layout-07-policy-zones",
        &server,
        RunOptions {
            output: &directory.path().join("first.pdf"),
            debug: None,
            cache: Some(&cache),
            model: "m2-identity-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: true,
        },
    );

    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    let first_events = parse_events(&first.stdout);
    assert_eq!(
        first_events
            .iter()
            .filter(|event| event["id"] == "translation_identity")
            .count(),
        2
    );
    assert!(!first_events.iter().any(|event| {
        event["id"] == "degradation_summary"
            && event["preserved_paragraph_count"]
                .as_u64()
                .unwrap_or_default()
                > 0
    }));
    assert_eq!(server.request_count(), 2);

    let calls_before = server.request_count();
    let second = run_openai(
        "unit-layout-07-policy-zones",
        &server,
        RunOptions {
            output: &directory.path().join("second.pdf"),
            debug: None,
            cache: Some(&cache),
            model: "m2-identity-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: true,
        },
    );

    assert_eq!(second.status.code(), Some(0));
    let second_events = parse_events(&second.stdout);
    assert_eq!(
        second_events
            .iter()
            .filter(|event| { event["event"] == "translation_cache" && event["status"] == "hit" })
            .count(),
        2
    );
    assert_eq!(
        second_events
            .iter()
            .filter(|event| event["id"] == "translation_identity")
            .count(),
        2
    );
    assert_eq!(
        server.request_count(),
        calls_before,
        "cached echo called API"
    );
    server.assert_clean();
}

#[test]
fn diagnostic_flood_keeps_other_ids_visible_and_reports_counts_by_id() {
    let directory = tempfile::tempdir().unwrap();
    let input = write_repeated_lines_pdf(directory.path(), 30);
    let layout = write_repeated_lines_layout(directory.path(), 30);
    let mut replies = vec![ScriptedReply::Echo; 28];
    replies.push(ScriptedReply::Output("龘"));
    replies.push(ScriptedReply::Output("{v999}"));
    let server = GateResponsesServer::start(replies);
    let output = run_openai_path(
        &input,
        Some(&layout),
        &server,
        RunOptions {
            output: &directory.path().join("translated.pdf"),
            debug: None,
            cache: None,
            model: "m2-diagnostic-flood-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: false,
        },
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let events = parse_events(&output.stdout);
    assert_eq!(
        events
            .iter()
            .filter(|event| event["id"] == "translation_identity")
            .count(),
        25,
        "requests={}, stdout={}",
        server.request_count(),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        events
            .iter()
            .any(|event| event["id"] == "unsupported_output_glyph")
    );
    assert!(
        events
            .iter()
            .any(|event| event["id"] == "placeholder_violation")
    );
    let dropped = events
        .iter()
        .find(|event| event["id"] == "dropped_diagnostics")
        .unwrap();
    assert_eq!(dropped["count"], 3);
    assert_eq!(
        dropped["counts_by_id"],
        serde_json::json!([{ "id": "translation_identity", "count": 3 }])
    );
    let summary = events
        .iter()
        .find(|event| event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["preserved_paragraph_count"], 2);
    let reasons = summary["preserved_paragraphs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|paragraph| paragraph["reason"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        reasons,
        BTreeSet::from(["placeholder_violation", "unsupported_font"])
    );
    assert_eq!(server.request_count(), 30);
    server.assert_clean();
}

#[test]
fn cjk_translation_overflow_preserves_the_original_paragraph() {
    const LONG_HAN: &str = "模型系统数据验证论文翻译语义结果保持结构流程稳定缓存重试诊断排版字体";

    let directory = tempfile::tempdir().unwrap();
    let debug = directory.path().join("debug");
    let output_path = directory.path().join("overflow.pdf");
    let server = GateResponsesServer::start([ScriptedReply::Output(LONG_HAN)]);
    let output = run_openai(
        "unit-base-01-single-line",
        &server,
        RunOptions {
            output: &output_path,
            debug: Some(&debug),
            cache: None,
            model: "m2-cjk-overflow-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: false,
        },
    );

    assert_single_preserved_paragraph(&output, "typeset_overflow");
    assert_eq!(
        translated_han_strings(&debug.join("06-translate.il.json")),
        [LONG_HAN]
    );
    assert!(translated_han_strings(&debug.join("07-typeset.il.json")).is_empty());
    assert_eq!(
        std::fs::read(&output_path).unwrap(),
        std::fs::read(
            repo_root()
                .join("corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf")
        )
        .unwrap()
    );
    server.assert_clean();
}

#[test]
fn math_shaped_fallback_text_bypasses_translation_without_hiding_prose() {
    const MATH_LINES: [&str; 4] = [
        "Attention(Q,K,V) = softmax(QK^T / sqrt(dk))V (1)",
        "Q",
        "dmodel×dk",
        "MultiHead(Q,K,V) = Concat(head1,...,headh)WO",
    ];
    const PROSE: &str = "This method improves translation quality across documents.";

    let directory = tempfile::tempdir().unwrap();
    let debug = directory.path().join("debug");
    let output_path = directory.path().join("math-passthrough.pdf");
    let server = GateResponsesServer::start([ScriptedReply::Echo]);
    let output = run_openai(
        "unit-form-01-math-shapes",
        &server,
        RunOptions {
            output: &output_path,
            debug: Some(&debug),
            cache: None,
            model: "m3-math-passthrough-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: true,
        },
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let requests = server
        .requests()
        .into_iter()
        .filter(|request| request.kind == RequestKind::ParagraphTranslation)
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].input.trim(), PROSE);
    for math in MATH_LINES {
        assert!(!requests[0].input.contains(math));
    }

    let events = parse_events(&output.stdout);
    let passthrough = events
        .iter()
        .filter(|event| event["id"] == "math_passthrough")
        .collect::<Vec<_>>();
    assert_eq!(passthrough.len(), MATH_LINES.len());
    for (reading_order, event) in passthrough.into_iter().enumerate() {
        assert_eq!(event["page_index"], 0);
        assert_eq!(event["paragraph_index"], reading_order);
        assert_eq!(event["reading_order"], reading_order);
        assert_eq!(
            event["source_characters"],
            MATH_LINES[reading_order].chars().count()
        );
    }
    assert!(
        events
            .iter()
            .all(|event| event["id"] != "degradation_summary"),
        "{}",
        serde_json::to_string_pretty(&events).unwrap()
    );

    let translate_il: serde_json::Value =
        serde_json::from_slice(&std::fs::read(debug.join("06-translate.il.json")).unwrap())
            .unwrap();
    let paragraphs = translate_il["pages"][0]["paragraphs"].as_array().unwrap();
    assert_eq!(paragraphs.len(), MATH_LINES.len() + 1);
    for (paragraph, expected) in paragraphs.iter().zip(MATH_LINES) {
        assert_eq!(paragraph["translated_text"], expected);
        let policies = paragraph["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .map(|character| character["layout"]["policy"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(policies, BTreeSet::from(["passthrough".to_owned()]));
    }
    let prose = paragraphs.last().unwrap();
    assert_eq!(prose["translated_text"], PROSE);
    assert_eq!(
        prose["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .map(|character| character["layout"]["policy"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["translate".to_owned()])
    );

    for extractor in ["poppler", "mupdf"] {
        let text = extract_pdf_text(&output_path, extractor);
        for expected in MATH_LINES {
            assert!(
                text.contains(expected),
                "{extractor} did not preserve {expected:?}: {text:?}"
            );
        }
    }
    let input_path =
        repo_root().join("corpus/fixtures/unit-form-01-math-shapes/unit-form-01-math-shapes.pdf");
    assert_eq!(
        decoded_page_streams(&output_path, 1),
        decoded_page_streams(&input_path, 1)
    );
    assert_valid_pdf(&output_path, "unit-form-01-math-shapes");
    server.assert_clean();
}

#[test]
fn malformed_nested_form_degrades_the_page_without_calling_the_backend() {
    let directory = tempfile::tempdir().unwrap();
    let input = write_nested_form_with_bad_matrix(directory.path());
    let output_path = directory.path().join("nested-form-output.pdf");
    let server = GateResponsesServer::start([]);
    let output = run_openai_path(
        &input,
        None,
        &server,
        RunOptions {
            output: &output_path,
            debug: None,
            cache: None,
            model: "m2-nested-form-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: false,
            strict: false,
        },
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let events = parse_events(&output.stdout);
    assert!(events.iter().any(|event| {
        event["id"] == "page_degraded"
            && event["page_index"] == 0
            && event["reason"] == "bad_form_matrix"
    }));
    let summary = events
        .iter()
        .find(|event| event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["degraded_page_indices"], serde_json::json!([0]));
    assert_eq!(server.request_count(), 0);
    assert_eq!(
        std::fs::read(&output_path).unwrap(),
        std::fs::read(&input).unwrap()
    );
    server.assert_clean();
}

#[test]
fn every_legal_fixture_uses_the_loopback_responses_gate() {
    let ids = all_fixture_ids();
    let unique_cases = ids
        .iter()
        .flat_map(|id| fixture_manifest(id).identity.cases)
        .collect::<BTreeSet<_>>();
    let legal = ids
        .iter()
        .filter(|id| fixture_manifest(id).identity.legality == "legal")
        .cloned()
        .collect::<BTreeSet<_>>();
    let rejected = BTreeSet::from([
        "intg-scan-10-nine-of-ten".to_owned(),
        "intg-scan-11-four-of-five".to_owned(),
        "intg-scan-12-image-with-blank-backs".to_owned(),
        "unit-doc-03-aes128-user-password".to_owned(),
        "unit-doc-03-rc4-empty-password".to_owned(),
        "unit-scan-01-image-only".to_owned(),
        "unit-scan-02-invisible-ocr".to_owned(),
    ]);
    assert_eq!(ids.len(), 143, "Corpus fixture inventory changed");
    assert_eq!(unique_cases.len(), 81, "Corpus case inventory changed");
    assert_eq!(legal.len(), 103, "legal fixture inventory changed");
    assert!(rejected.is_subset(&legal));

    let directory = tempfile::tempdir().unwrap();
    let server = GateResponsesServer::start([]);
    let mut output_count = 0usize;
    for id in legal {
        let output_path = directory.path().join(format!("{id}.pdf"));
        let calls_before = server.request_count();
        let output = run_openai(
            &id,
            &server,
            RunOptions {
                output: &output_path,
                debug: None,
                cache: None,
                model: "m2-corpus-model",
                target_language: "zh-CN",
                glossary: None,
                auto_terms: false,
                strict: false,
            },
        );
        assert!(
            output.stderr.is_empty(),
            "fixture {id}: JSON stderr was not empty"
        );
        assert_secret_absent(&output.stdout, &format!("fixture {id} stdout"));
        let events = parse_events(&output.stdout);
        let resolved = events
            .iter()
            .find(|event| event["event"] == "configuration_resolved")
            .unwrap_or_else(|| panic!("fixture {id}: no resolved configuration"));
        assert_eq!(resolved["backend"], "openai", "fixture {id}");
        assert_eq!(resolved["model"], "m2-corpus-model", "fixture {id}");

        if rejected.contains(&id) {
            assert_eq!(output.status.code(), Some(2), "fixture {id}");
            assert_terminal(&events, "error");
            assert_eq!(events.last().unwrap()["category"], "input", "fixture {id}");
            assert!(
                !output_path.exists(),
                "fixture {id} produced rejected output"
            );
            assert_eq!(
                server.request_count(),
                calls_before,
                "fixture {id} called backend"
            );
        } else {
            assert_eq!(
                output.status.code(),
                Some(0),
                "fixture {id}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_terminal(&events, "result");
            assert!(output_path.is_file(), "fixture {id} produced no output");
            assert_valid_pdf(&output_path, &id);
            assert!(
                !events.iter().any(|event| {
                    event["id"] == "degradation_summary"
                        && event["preserved_paragraphs"]
                            .as_array()
                            .is_some_and(|paragraphs| {
                                paragraphs.iter().any(|paragraph| {
                                    matches!(
                                        paragraph["reason"].as_str(),
                                        Some("translation_failure" | "placeholder_violation")
                                    )
                                })
                            })
                }),
                "fixture {id} degraded a deterministic translation: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            output_count += 1;
        }
    }
    assert_eq!(output_count, 96);
    assert_eq!(
        server.request_count(),
        128,
        "eligible corpus request inventory changed"
    );
    assert!(server.requests().iter().all(|request| {
        request.path == "/v1/responses"
            && request.model == "m2-corpus-model"
            && !request.input.is_empty()
    }));
    server.assert_clean();
}

#[test]
fn cache_retry_invalidation_degradation_events_and_secrets_close_the_m2_matrix() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("translations.redb");
    let first_debug = directory.path().join("first-debug");
    let server = GateResponsesServer::start([ScriptedReply::Status("429 Too Many Requests")]);
    let first = run_openai(
        "unit-base-01-single-line",
        &server,
        RunOptions {
            output: &directory.path().join("first.pdf"),
            debug: Some(&first_debug),
            cache: Some(&cache),
            model: "m2-cache-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: true,
            strict: false,
        },
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    let first_events = parse_events(&first.stdout);
    assert_terminal(&first_events, "result");
    assert!(
        first_events
            .iter()
            .any(|event| { event["event"] == "translation_cache" && event["status"] == "miss" })
    );
    assert!(
        first_events
            .iter()
            .any(|event| { event["event"] == "diagnostic" && event["id"] == "translation_retry" })
    );
    assert!(
        first_events
            .iter()
            .any(|event| event["event"] == "page_progress")
    );
    assert!(
        first_events
            .iter()
            .any(|event| event["event"] == "configuration_resolved")
    );
    assert!(
        first_events
            .iter()
            .any(|event| event["event"] == "stage_started")
    );
    assert_eq!(
        server.request_count(),
        3,
        "term extraction plus one transient paragraph retry must make three calls"
    );
    assert_eq!(server.request_count_by_kind(RequestKind::TermExtraction), 1);
    assert_eq!(
        server.request_count_by_kind(RequestKind::ParagraphTranslation),
        2
    );

    let second_calls = server.request_count();
    let second = run_openai(
        "unit-base-01-single-line",
        &server,
        RunOptions {
            output: &directory.path().join("second.pdf"),
            debug: None,
            cache: Some(&cache),
            model: "m2-cache-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: true,
            strict: false,
        },
    );
    assert!(second.status.success());
    let second_events = parse_events(&second.stdout);
    assert!(
        second_events
            .iter()
            .any(|event| { event["event"] == "translation_cache" && event["status"] == "hit" })
    );
    assert_eq!(
        server.request_count(),
        second_calls,
        "second run called the API"
    );

    for (name, model, target) in [
        ("model", "m2-cache-model-v2", "zh-CN"),
        ("target", "m2-cache-model", "ja-JP"),
    ] {
        let before = server.request_count();
        let changed = run_openai(
            "unit-base-01-single-line",
            &server,
            RunOptions {
                output: &directory.path().join(format!("{name}.pdf")),
                debug: None,
                cache: Some(&cache),
                model,
                target_language: target,
                glossary: None,
                auto_terms: true,
                strict: false,
            },
        );
        assert!(changed.status.success(), "{name} invalidation failed");
        assert_eq!(
            server.request_count(),
            before + 2,
            "{name} did not invalidate automatic terms and paragraph translation"
        );
    }

    let glossary = directory.path().join("glossary.toml");
    std::fs::write(
        &glossary,
        "version = 1\n[[terms]]\nsource = 'MIMUS'\ntarget = '米姆斯'\n",
    )
    .unwrap();
    let before_glossary = server.request_count();
    let glossary_run = run_openai(
        "unit-base-01-single-line",
        &server,
        RunOptions {
            output: &directory.path().join("glossary.pdf"),
            debug: None,
            cache: Some(&cache),
            model: "m2-cache-model",
            target_language: "zh-CN",
            glossary: Some(&glossary),
            auto_terms: true,
            strict: false,
        },
    );
    assert!(glossary_run.status.success());
    assert_eq!(server.request_count(), before_glossary + 1);
    assert_eq!(
        server.request_count_by_kind(RequestKind::TermExtraction),
        3,
        "user glossary changes must reuse the cached automatic glossary"
    );
    assert_eq!(
        server.request_count_by_kind(RequestKind::ParagraphTranslation),
        5
    );
    assert_eq!(server.request_count(), 8);
    server.assert_clean();

    let violation_debug = directory.path().join("violation-debug");
    let violation_server = GateResponsesServer::start([ScriptedReply::Output("{v999}")]);
    let violation = run_openai(
        "unit-base-01-single-line",
        &violation_server,
        RunOptions {
            output: &directory.path().join("violation.pdf"),
            debug: Some(&violation_debug),
            cache: None,
            model: "m2-violation-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: true,
            strict: false,
        },
    );
    assert!(violation.status.success());
    assert_single_preserved_paragraph(&violation, "placeholder_violation");
    let violation_events = parse_events(&violation.stdout);
    let diagnostic = violation_events
        .iter()
        .find(|event| event["id"] == "placeholder_violation")
        .unwrap();
    assert_eq!(diagnostic["violation"], "unknown");
    let summary = violation_events
        .iter()
        .find(|event| event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(
        summary["preserved_paragraphs"][0]["placeholder_violation"],
        "unknown"
    );
    assert_eq!(
        std::fs::read(directory.path().join("violation.pdf")).unwrap(),
        std::fs::read(
            repo_root()
                .join("corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf")
        )
        .unwrap()
    );
    assert_eq!(violation_server.request_count(), 2);
    violation_server.assert_clean();

    let delayed_server = GateResponsesServer::start([ScriptedReply::DelayedOutput(
        Duration::from_millis(10),
        "中",
    )]);
    let delayed = run_openai(
        "unit-base-01-single-line",
        &delayed_server,
        RunOptions {
            output: &directory.path().join("delayed.pdf"),
            debug: None,
            cache: None,
            model: "m2-delayed-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: true,
            strict: false,
        },
    );
    assert!(delayed.status.success());
    assert_eq!(delayed_server.request_count(), 2);
    delayed_server.assert_clean();

    let malformed_server = GateResponsesServer::start([ScriptedReply::Body("{}")]);
    let malformed = run_openai(
        "unit-base-01-single-line",
        &malformed_server,
        RunOptions {
            output: &directory.path().join("malformed.pdf"),
            debug: None,
            cache: None,
            model: "m2-malformed-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: true,
            strict: false,
        },
    );
    assert_single_preserved_paragraph(&malformed, "translation_failure");
    assert_eq!(malformed_server.request_count(), 2);
    malformed_server.assert_clean();

    let disconnect_server = GateResponsesServer::start([ScriptedReply::Disconnect]);
    let disconnected = run_openai(
        "unit-base-01-single-line",
        &disconnect_server,
        RunOptions {
            output: &directory.path().join("disconnected.pdf"),
            debug: None,
            cache: None,
            model: "m2-disconnect-model",
            target_language: "zh-CN",
            glossary: None,
            auto_terms: true,
            strict: false,
        },
    );
    assert_single_preserved_paragraph(&disconnected, "translation_failure");
    assert_eq!(disconnect_server.request_count(), 2);
    disconnect_server.assert_clean();

    for (name, output) in [
        ("first stdout", &first.stdout),
        ("first stderr", &first.stderr),
        ("second stdout", &second.stdout),
        ("second stderr", &second.stderr),
        ("glossary stdout", &glossary_run.stdout),
        ("violation stdout", &violation.stdout),
        ("violation stderr", &violation.stderr),
        ("delayed stdout", &delayed.stdout),
        ("delayed stderr", &delayed.stderr),
        ("malformed stdout", &malformed.stdout),
        ("malformed stderr", &malformed.stderr),
        ("disconnected stdout", &disconnected.stdout),
        ("disconnected stderr", &disconnected.stderr),
    ] {
        assert_secret_absent(output, name);
    }
    assert_tree_has_no_secret(directory.path());
}
