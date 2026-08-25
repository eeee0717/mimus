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

enum ScriptedReply {
    Status(&'static str),
    Output(&'static str),
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
    let mut output = String::new();
    let mut rest = input;
    let mut emitted_text = false;
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
        if segment.chars().any(|character| !character.is_whitespace()) {
            output.push(if input.trim() == "中" { '文' } else { '中' });
            emitted_text = true;
        }
        rest = &rest[segment.len()..];
    }
    if !emitted_text && output.is_empty() {
        output.push('中');
    }
    output
}

#[test]
fn deterministic_fake_translation_preserves_indexed_protocol_markers() {
    assert_eq!(
        deterministic_translation("Alpha <b1>bold</b1> {v2} literal {l3}v1}"),
        "中<b1>中</b1>{v2}中{l3}中"
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
}

fn run_openai(id: &str, server: &GateResponsesServer, options: RunOptions<'_>) -> Output {
    let input = repo_root()
        .join("corpus/fixtures")
        .join(id)
        .join(format!("{id}.pdf"));
    let config_file = options.output.parent().unwrap().join("absent-config.toml");
    let mut command = Command::new(BIN);
    command
        .env(PDFIUM_ENV, pdfium_library())
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
    if let Some(debug) = options.debug {
        command.arg("--debug").arg(debug);
    }
    if let Some(glossary) = options.glossary {
        command.arg("--glossary").arg(glossary);
    }
    let recording = repo_root()
        .join("corpus/layout-recordings")
        .join(format!("{id}.json"));
    if recording.is_file() {
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
    assert_eq!(ids.len(), 142, "Corpus fixture inventory changed");
    assert_eq!(unique_cases.len(), 80, "Corpus case inventory changed");
    assert_eq!(legal.len(), 102, "legal fixture inventory changed");
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
    assert_eq!(output_count, 95);
    assert_eq!(
        server.request_count(),
        131,
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
        },
    );
    assert!(violation.status.success());
    assert_single_preserved_paragraph(&violation, "placeholder_violation");
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
