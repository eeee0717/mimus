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
use sha2::{Digest, Sha256};

const BIN: &str = env!("CARGO_BIN_EXE_mimus");
const PDFIUM_ENV: &str = "MIMUS_PDFIUM_LIBRARY";
const WRITE_FAULT_ENV: &str = "MIMUS_TEST_WRITE_FAULT";

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
                        stream.set_nonblocking(false).unwrap();
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

    fn wait_for_requests(&self, expected: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let requests = self.requests();
            if requests.len() >= expected || std::time::Instant::now() >= deadline {
                return requests;
            }
            thread::sleep(Duration::from_millis(1));
        }
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
            r#"{"error":"injected table-cell failure"}"#.to_owned(),
        )
    } else if input == "Scale by √{v1}, then continue." {
        ("200 OK", r#"{"output_text":"A√B{v1}."}"#.to_owned())
    } else if input == "MIMUS MIMUS MIMUS MIMUS" {
        (
            "200 OK",
            serde_json::json!({
                "output_text": "模型数据验证论文翻译结果保持结构流程稳定缓存重试诊断排版字体安全边界模型数据验证论文翻译结果保持结构流程稳定缓存重试诊断排版字体安全边界模型数据验证论文翻译结果保持结构流程稳定缓存重试诊断排版字体安全边界"
            })
            .to_string(),
        )
    } else if input == "MIMUS {v1} MIMUS" {
        (
            "200 OK",
            serde_json::json!({ "output_text": "模型 {v1} 模型" }).to_string(),
        )
    } else if input == "MIMUS CIMUS" {
        (
            "200 OK",
            serde_json::json!({ "output_text": "模型ϵM" }).to_string(),
        )
    } else if input == "For all {v1} in {v2}, the model agrees." {
        (
            "200 OK",
            serde_json::json!({ "output_text": "对于 {v1} 和 {v2}，模型成立。" }).to_string(),
        )
    } else if input == "Inter-script spacing stays explicit." {
        (
            "200 OK",
            serde_json::json!({ "output_text": "模B模" }).to_string(),
        )
    } else if input == "MIMUS I" {
        (
            "200 OK",
            serde_json::json!({ "output_text": "甲乙丙丁戊（）己庚。" }).to_string(),
        )
    } else if input == "First control paragraph." {
        (
            "200 OK",
            serde_json::json!({ "output_text": "模型" }).to_string(),
        )
    } else if input == "Second conflict paragraph." {
        (
            "200 OK",
            serde_json::json!({
                "output_text": "模型数据验证论文翻译结果保持结构流程稳定缓存重试诊断排版字体安全边界模型数据验证论文翻译结果保持结构流程稳定缓存重试诊断排版字体安全边界模型数据验证论文翻译结果保持结构流程稳定缓存重试诊断排版字体安全边界"
            })
            .to_string(),
        )
    } else if input == "Third control paragraph." {
        (
            "200 OK",
            serde_json::json!({ "output_text": "数据" }).to_string(),
        )
    } else if input == "M M M" {
        (
            "200 OK",
            serde_json::json!({
                "output_text": "模型数据验证论文翻译结果保持结构流程稳定"
            })
            .to_string(),
        )
    } else if input == "MIIMIIMIIM" {
        (
            "200 OK",
            serde_json::json!({ "output_text": "模型" }).to_string(),
        )
    } else if input == "The result {v1} = 1, 2, 3 shows the distinction." {
        (
            "200 OK",
            serde_json::json!({
                "output_text": "该结果 {v1} = 1, 2, 3 展示了区别。"
            })
            .to_string(),
        )
    } else if input == "Compare {v1} with the reference." {
        (
            "200 OK",
            serde_json::json!({ "output_text": "比较 {v1} 与参考值。" }).to_string(),
        )
    } else if input == "• Compare {v1} with the reference." {
        (
            "200 OK",
            serde_json::json!({ "output_text": "• 比较 {v1} 与参考值。" }).to_string(),
        )
    } else if input == "In 2024, we measured 3.14 and 1, 2, 3." {
        (
            "200 OK",
            serde_json::json!({
                "output_text": "在 2024 年，我们测得 3.14 以及 1, 2, 3。"
            })
            .to_string(),
        )
    } else if input == "Regular <b1>strong</b1> emphasis (small note) x2 end." {
        (
            "200 OK",
            serde_json::json!({
                "output_text": "常规 <b1>粗体</b1> 强调（小注）x2 结束。"
            })
            .to_string(),
        )
    } else if input.starts_with("r<b1>B</b1>") && input.matches("<b").count() == 45 {
        let output_text = (1..=45)
            .map(|index| format!("<b{index}>粗</b{index}>"))
            .collect::<String>();
        (
            "200 OK",
            serde_json::json!({ "output_text": output_text }).to_string(),
        )
    } else if input
        == "The ratio{v1} remains attached while this narrow paragraph wraps onto a second line."
    {
        (
            "200 OK",
            serde_json::json!({
                "output_text": "这个经过显著扩展并为了验证狭窄多行重排而刻意写长的译文比值 {v1} 始终保持分子分母和分数线完整连接。"
            })
            .to_string(),
        )
    } else if matches!(
        input.as_str(),
        "1204 ops" | "1198 ops" | "8.1 ms" | "8.3 ms"
    ) {
        let tokens = mimus_quality_contract::conserved_tokens(&input).join(" ");
        (
            "200 OK",
            serde_json::json!({ "output_text": format!("M{tokens}") }).to_string(),
        )
    } else {
        ("200 OK", r#"{"output_text":"M"}"#.to_owned())
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

#[derive(Debug, Deserialize)]
struct SemanticDigestBaseline {
    version: u32,
    fixture_count: usize,
    case_count: usize,
    snapshot_counts: BTreeMap<String, usize>,
    snapshots: BTreeMap<String, String>,
}

fn update_snapshot_digests(
    digests: &mut BTreeMap<String, Sha256>,
    counts: &mut BTreeMap<String, usize>,
    lane: &str,
    directory: &Path,
    fixture_id: &str,
) {
    for name in snapshot_names(directory) {
        let stage = name.strip_suffix(".il.json").unwrap();
        let key = format!("{lane}/{stage}");
        let mut snapshot: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join(&name)).unwrap()).unwrap();
        quantize_semantic_snapshot(&mut snapshot);
        canonicalize_platform_substituted_font_ink(fixture_id, &mut snapshot);
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let digest = digests.entry(key.clone()).or_default();
        digest.update((fixture_id.len() as u64).to_be_bytes());
        digest.update(fixture_id.as_bytes());
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
        *counts.entry(key).or_default() += 1;
    }
}

fn quantize_semantic_snapshot(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                quantize_semantic_snapshot(value);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                quantize_semantic_snapshot(value);
            }
        }
        serde_json::Value::Number(number) if number.as_i64().is_none() => {
            let original = number.as_f64().unwrap();
            let quantized = (original * 1000.0).round() / 1000.0;
            *number = serde_json::Number::from_f64(if quantized == -0.0 { 0.0 } else { quantized })
                .unwrap();
        }
        _ => {}
    }
}

fn canonicalize_platform_substituted_font_ink(fixture_id: &str, snapshot: &mut serde_json::Value) {
    if !matches!(
        fixture_id,
        "unit-cmap-10-differences-agl-type1" | "unit-font-01-std14-custom-widths"
    ) {
        return;
    }

    for page in snapshot["pages"].as_array_mut().unwrap() {
        for paragraph in page["paragraphs"].as_array_mut().unwrap() {
            let paragraph_bounds = paragraph["bounds"].clone();
            for character in paragraph["text"]["chars"].as_array_mut().unwrap() {
                character["visual_bbox"] = character["box"].clone();
                character["layout"]["bounds"] = paragraph_bounds.clone();
            }
        }
    }
}

#[test]
fn semantic_digest_canonicalizes_only_declared_platform_font_ink() {
    let snapshot = || {
        serde_json::json!({
            "pages": [{
                "paragraphs": [{
                    "bounds": {"left": 1.0, "bottom": 2.0, "right": 3.0, "top": 4.0},
                    "text": {"chars": [{
                        "box": {"left": 5.0, "bottom": 6.0, "right": 7.0, "top": 8.0},
                        "visual_bbox": {"left": 9.0, "bottom": 10.0, "right": 11.0, "top": 12.0},
                        "layout": {"bounds": {"left": 13.0, "bottom": 14.0, "right": 15.0, "top": 16.0}}
                    }]}
                }]
            }]
        })
    };
    let mut canonical = snapshot();
    let mut ordinary = snapshot();

    canonicalize_platform_substituted_font_ink("unit-font-01-std14-custom-widths", &mut canonical);
    canonicalize_platform_substituted_font_ink("unit-base-01-single-line", &mut ordinary);

    let character = &canonical["pages"][0]["paragraphs"][0]["text"]["chars"][0];
    assert_eq!(character["visual_bbox"], character["box"]);
    assert_eq!(
        character["layout"]["bounds"],
        canonical["pages"][0]["paragraphs"][0]["bounds"]
    );
    assert_eq!(ordinary, snapshot());
}

fn assert_semantic_digest_baseline(
    fixture_count: usize,
    case_count: usize,
    counts: BTreeMap<String, usize>,
    digests: BTreeMap<String, Sha256>,
) {
    let expected: SemanticDigestBaseline =
        toml::from_str(include_str!("fixtures/m3-semantic-digests-v1.toml")).unwrap();
    let actual = digests
        .into_iter()
        .map(|(stage, digest)| (stage, format!("{:x}", digest.finalize())))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(expected.version, 1);
    assert_eq!(expected.fixture_count, fixture_count);
    assert_eq!(expected.case_count, case_count);
    assert_eq!(expected.snapshot_counts, counts);
    assert_eq!(expected.snapshots, actual);
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

fn run_none_with_write_fault(input: &Path, output: &Path, fault: &str) -> Output {
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    configure_test_fonts(&mut command);
    command
        .env(WRITE_FAULT_ENV, fault)
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("OPENAI_API_KEY", "must-not-be-used")
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--layout",
            "single-line",
            "--output",
        ])
        .arg(output)
        .arg(input)
        .output()
        .unwrap()
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

fn run_openai_with_layout(
    id: &str,
    server: &FakeResponsesServer,
    output: &Path,
    debug: &Path,
) -> Output {
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    configure_test_fonts(&mut command);
    command
        .env("API_KEY", "mimus-form-corpus-test-key")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--json",
            "translate",
            "--backend",
            "openai",
            "--endpoint",
            &server.endpoint,
            "--model",
            "form-corpus-test-model",
            "--no-auto-terms",
            "--no-cache",
            "--layout-replay",
        ])
        .arg(layout_recording_path(id))
        .arg("--debug")
        .arg(debug)
        .arg("--output")
        .arg(output)
        .arg(fixture_path(id))
        .output()
        .unwrap()
}

fn assert_request_count_after_fixture(
    id: &str,
    server: &FakeResponsesServer,
    expected: usize,
    output: &Output,
    debug: &Path,
) {
    let requests = server.wait_for_requests(expected);
    if requests.len() != expected {
        let preserved = std::fs::read(debug.join("09-write.il.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|il| {
                il["pages"]
                    .as_array()?
                    .iter()
                    .flat_map(|page| page["paragraphs"].as_array().into_iter().flatten())
                    .map(|paragraph| paragraph["preserved"].clone())
                    .find(|reason| !reason.is_null())
            });
        panic!(
            "fixture {id} expected {expected} cumulative request(s), captured {requests:?}, preserved={preserved:?}\nstderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        );
    }
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

fn page_font_resource_names(path: &Path, page_number: u32) -> BTreeSet<String> {
    let document = lopdf::Document::load(path).unwrap();
    let page_id = document.get_pages()[&page_number];
    let page = document.get_dictionary(page_id).unwrap();
    let resources = page
        .get_deref(b"Resources", &document)
        .unwrap()
        .as_dict()
        .unwrap();
    let fonts = resources
        .get_deref(b"Font", &document)
        .unwrap()
        .as_dict()
        .unwrap();
    fonts
        .iter()
        .map(|(name, _)| String::from_utf8(name.clone()).unwrap())
        .collect()
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
fn request_timeout_rejects_every_value_outside_one_through_six_hundred() {
    for value in ["-1", "0", "601"] {
        let output = Command::new(BIN)
            .env_remove("MIMUS_REQUEST_TIMEOUT")
            .env("BASE_URL", "http://timeout-endpoint-canary.invalid")
            .env("API_KEY", "timeout-key-canary")
            .args([
                "--json",
                "translate",
                "--backend",
                "none",
                &format!("--request-timeout={value}"),
            ])
            .arg(fixture())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let events = parse_events(&output.stdout);
        assert_one_terminal_last(&events, "error");
        assert_eq!(events[0]["reason"], "invalid_arguments");
        let rendered = String::from_utf8_lossy(&output.stdout);
        assert!(!rendered.contains("timeout-endpoint-canary"));
        assert!(!rendered.contains("timeout-key-canary"));
    }

    let output = Command::new(BIN)
        .env("MIMUS_REQUEST_TIMEOUT", "not-an-integer")
        .args(["--json", "translate", "--backend", "none"])
        .arg(fixture())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));

    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(&config, "request_timeout_secs = 601\n").unwrap();
    let output = Command::new(BIN)
        .env("MIMUS_CONFIG_FILE", config)
        .env_remove("MIMUS_REQUEST_TIMEOUT")
        .args(["--json", "translate", "--backend", "none"])
        .arg(fixture())
        .output()
        .unwrap();
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
        "backend = 'none'\nbase_url = 'https://file.invalid'\nmodel = 'file-model'\ntarget_language = 'file-language'\nfont_regular = 'file-regular.ttf'\nfont_bold = 'file-bold.ttf'\nfont_fallback_regular = 'file-fallback-regular.ttf'\nfont_fallback_bold = 'file-fallback-bold.ttf'\ncache = 'file-cache.redb'\nconcurrency = 2\nrequest_timeout_secs = 180\n",
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
        .env("MIMUS_REQUEST_TIMEOUT", "invalid-but-overridden")
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
            "--request-timeout",
            "240",
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
    assert_eq!(resolved["request_timeout_secs"], 240);
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
    assert_eq!(resolved["request_timeout_secs"], 120);
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
fn corpus_inventory_runs_every_fixture_through_bounded_production_paths() {
    let ids = all_fixture_ids();
    let cases = ids
        .iter()
        .flat_map(|id| fixture_manifest(id).identity.cases)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 201, "Corpus fixture inventory changed");
    assert_eq!(cases.len(), 128, "Corpus case inventory changed");

    let mut snapshot_digests = BTreeMap::new();
    let mut snapshot_counts = BTreeMap::new();

    for id in &ids {
        let input = fixture_path(id);
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
        assert_parseable_snapshots(&inspect_debug, id);
        update_snapshot_digests(
            &mut snapshot_digests,
            &mut snapshot_counts,
            "inspect",
            &inspect_debug,
            id,
        );
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
                Some(fixture_manifest(id).page.len() as u64),
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
        assert_parseable_snapshots(&translate_debug, id);
        update_snapshot_digests(
            &mut snapshot_digests,
            &mut snapshot_counts,
            "translate",
            &translate_debug,
            id,
        );

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
            assert_none_translation_identity(&translate_snapshot, id);
            assert!(translated.is_file(), "fixture {id}: no translated output");
            let input_bytes = std::fs::read(&input).unwrap();
            let output_bytes = std::fs::read(&translated).unwrap();
            assert!(output_bytes.starts_with(&input_bytes), "fixture {id}");
            let qpdf = Command::new("qpdf")
                .arg("--check")
                .arg(&translated)
                .output()
                .unwrap();
            let legality = fixture_manifest(id).identity.legality;
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

    assert_semantic_digest_baseline(ids.len(), cases.len(), snapshot_counts, snapshot_digests);
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
fn one_model_region_with_two_author_columns_preserves_column_ownership() {
    let output = run_inspect_with_layout("unit-para-17-author-columns");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 2, "{paragraphs:#?}");

    let summaries = paragraphs
        .iter()
        .map(|paragraph| {
            let chars = paragraph["text"]["chars"].as_array().unwrap();
            let text = chars
                .iter()
                .filter_map(|character| character["unicode"].as_str())
                .collect::<String>();
            let left = chars
                .iter()
                .map(|character| character["box"]["left"].as_f64().unwrap())
                .min_by(f64::total_cmp)
                .unwrap();
            let right = chars
                .iter()
                .map(|character| character["box"]["right"].as_f64().unwrap())
                .max_by(f64::total_cmp)
                .unwrap();
            let layout_left = chars
                .iter()
                .map(|character| character["layout"]["bounds"]["left"].as_f64().unwrap())
                .min_by(f64::total_cmp)
                .unwrap();
            let layout_right = chars
                .iter()
                .map(|character| character["layout"]["bounds"]["right"].as_f64().unwrap())
                .max_by(f64::total_cmp)
                .unwrap();
            (text, left, right, layout_left, layout_right)
        })
        .collect::<Vec<_>>();
    assert_eq!(summaries[0].0, "MMM");
    assert_eq!(summaries[1].0, "MMM");
    assert!(
        summaries[0].2 < summaries[1].1,
        "author columns overlap or interleave: {summaries:?}"
    );
    assert!(
        summaries[0].4 <= summaries[1].3,
        "author column typeset containers overlap: {summaries:?}"
    );
}

#[test]
fn scaled_text_matrix_uses_page_space_em_and_retained_rule_blocks_expansion() {
    let id = "unit-para-18-scaled-tm-rule";
    let inspected = run_inspect_with_layout(id);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 1, "{paragraphs:#?}");
    assert_eq!(il_paragraph_text(&paragraphs[0]), "MIMUS MIMUS MIMUS MIMUS");
    assert!(
        paragraphs[0]["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| { (character["font_size"].as_f64().unwrap() - 12.0).abs() <= 0.001 })
    );

    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("scaled-tm-rule.pdf");
    let server = FakeResponsesServer::start();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let translated = command
        .env(PDFIUM_ENV, pdfium_library())
        .env("API_KEY", "mimus-scaled-tm-test-key")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--json",
            "translate",
            "--backend",
            "openai",
            "--endpoint",
            &server.endpoint,
            "--model",
            "scaled-tm-test-model",
            "--no-auto-terms",
            "--no-cache",
            "--layout-replay",
        ])
        .arg(layout_recording_path(id))
        .arg("--output")
        .arg(&output_path)
        .arg(fixture_path(id))
        .output()
        .unwrap();
    assert!(
        translated.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&translated.stderr),
        String::from_utf8_lossy(&translated.stdout)
    );
    assert_eq!(server.requests(), vec!["MIMUS MIMUS MIMUS MIMUS"]);
    let events = parse_events(&translated.stdout);
    assert!(
        events.iter().any(|event| {
            event["event"] == "diagnostic"
                && event["id"] == "typeset_overflow_detail"
                && event["obstacle_count"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
        }),
        "{events:#?}"
    );
    let summary = events
        .iter()
        .find(|event| event["event"] == "diagnostic" && event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["preserved_paragraph_count"], 1);
    assert_eq!(
        summary["preserved_paragraphs"][0]["reason"],
        "typeset_overflow"
    );
    assert!(output_path.is_file());
}

#[test]
fn whitespace_only_fixture_is_local_identity_without_a_backend_request_or_new_ink() {
    let id = "unit-translation-02-whitespace-identity";
    let inspected = run_inspect_with_layout(id);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 1, "{paragraphs:#?}");
    assert_eq!(il_paragraph_text(&paragraphs[0]), " ");

    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("whitespace-identity.pdf");
    let server = FakeResponsesServer::start();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let translated = command
        .env(PDFIUM_ENV, pdfium_library())
        .env("API_KEY", "mimus-whitespace-identity-test-key")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--json",
            "translate",
            "--backend",
            "openai",
            "--endpoint",
            &server.endpoint,
            "--model",
            "whitespace-identity-test-model",
            "--no-auto-terms",
            "--no-cache",
            "--layout-replay",
        ])
        .arg(layout_recording_path(id))
        .arg("--output")
        .arg(&output_path)
        .arg(fixture_path(id))
        .output()
        .unwrap();
    assert!(
        translated.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&translated.stderr),
        String::from_utf8_lossy(&translated.stdout)
    );
    assert!(server.requests().is_empty());
    let events = parse_events(&translated.stdout);
    assert_one_terminal_last(&events, "result");
    assert!(!events.iter().any(|event| {
        event["event"] == "diagnostic" && event["id"] == "typeset_overflow_detail"
    }));
    let input_bytes = std::fs::read(fixture_path(id)).unwrap();
    let output_bytes = std::fs::read(&output_path).unwrap();
    assert!(output_bytes.starts_with(&input_bytes));
}

#[test]
fn page_zero_title_and_bounded_author_block_are_policy_passthrough() {
    let output = run_inspect_with_recording(
        "unit-para-17-author-columns",
        "unit-para-17-title-author-abstract",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 3, "{paragraphs:#?}");
    for paragraph in &paragraphs[..2] {
        assert_eq!(paragraph.get("first_line_indent"), None, "{paragraph:#?}");
        assert!(
            paragraph["text"]["chars"]
                .as_array()
                .unwrap()
                .iter()
                .all(|character| character["layout"]["policy"] == "passthrough"),
            "{paragraph:#?}"
        );
    }
    assert!(
        paragraphs[2]["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| character["layout"]["policy"] == "translate"),
        "{:#?}",
        paragraphs[2]
    );

    let missing_anchor = run_inspect_with_recording(
        "unit-para-17-author-columns",
        "unit-para-17-title-without-lower-anchor",
    );
    assert!(
        missing_anchor.status.success(),
        "{}",
        String::from_utf8_lossy(&missing_anchor.stderr)
    );
    let events = parse_events(&missing_anchor.stdout);
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert!(
        paragraphs[1..]
            .iter()
            .flat_map(|paragraph| paragraph["text"]["chars"].as_array().unwrap())
            .all(|character| character["layout"]["policy"] == "translate"),
        "{paragraphs:#?}"
    );
}

#[test]
fn page_zero_author_geometry_ignores_reading_order_and_excludes_outside_body() {
    let output = run_inspect_with_recording(
        "unit-para-17-author-columns",
        "unit-para-17-title-author-reordered",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();

    let abstract_order = paragraphs
        .iter()
        .find(|paragraph| paragraph["text"]["chars"][0]["layout"]["label"] == "abstract")
        .unwrap()["reading_order"]
        .as_u64()
        .unwrap();
    let authors = paragraphs
        .iter()
        .filter(|paragraph| {
            paragraph["text"]["chars"][0]["layout"]["label"] == "fallback_line"
                && paragraph["bounds"]["bottom"].as_f64().unwrap() > 112.0
                && paragraph["bounds"]["top"].as_f64().unwrap() < 136.0
        })
        .collect::<Vec<_>>();
    assert_eq!(authors.len(), 2, "{paragraphs:#?}");
    for author in authors {
        assert!(
            author["reading_order"].as_u64().unwrap() > abstract_order,
            "fallback authors must follow the model abstract in reading order: {author:#?}"
        );
        assert!(
            author["text"]["chars"]
                .as_array()
                .unwrap()
                .iter()
                .all(|character| character["layout"]["policy"] == "passthrough"),
            "{author:#?}"
        );
    }

    let outside_body = paragraphs
        .iter()
        .find(|paragraph| paragraph["text"]["chars"][0]["layout"]["reading_order"] == 3)
        .unwrap();
    assert!(
        outside_body["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| character["layout"]["policy"] == "translate"),
        "{outside_body:#?}"
    );
}

#[test]
fn title_and_author_passthrough_skip_translation_and_keep_source_identity() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("title-author.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let output = command
        .env(PDFIUM_ENV, pdfium_library())
        .env("API_KEY", "mimus-title-author-test-key")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--json",
            "translate",
            "--backend",
            "openai",
            "--endpoint",
            &server.endpoint,
            "--model",
            "title-author-test-model",
            "--no-auto-terms",
            "--no-cache",
            "--layout-replay",
        ])
        .arg(layout_recording_path("unit-para-17-title-author-abstract"))
        .arg("--debug")
        .arg(&debug)
        .arg("--output")
        .arg(&output_path)
        .arg(fixture_path("unit-para-17-author-columns"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.requests(), vec!["M M"]);

    let read_il = |stage: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(debug.join(stage)).unwrap()).unwrap()
    };
    let before = read_il("03-paragraph_find.il.json");
    let after = read_il("09-write.il.json");
    for paragraph_index in 0..=1 {
        let before_chars = before["pages"][0]["paragraphs"][paragraph_index]["text"]["chars"]
            .as_array()
            .unwrap();
        let after_chars = after["pages"][0]["paragraphs"][paragraph_index]["text"]["chars"]
            .as_array()
            .unwrap();
        assert_eq!(before_chars.len(), after_chars.len());
        for (before, after) in before_chars.iter().zip(after_chars) {
            assert_eq!(after["unicode"], before["unicode"]);
            assert_eq!(after["passthrough"], before["passthrough"]);
            assert_eq!(after["font"], before["font"]);
            assert_eq!(after["font_size"], before["font_size"]);
            assert_eq!(after["baseline_origin"], before["baseline_origin"]);
            assert_eq!(after["box"], before["box"]);
            assert_eq!(after["visual_bbox"], before["visual_bbox"]);
        }
    }
}

#[test]
fn partial_model_formula_regions_are_completed_before_translation() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("formula-boundary.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let output = command
        .env(PDFIUM_ENV, pdfium_library())
        .env("API_KEY", "mimus-formula-boundary-test-key")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--json",
            "translate",
            "--backend",
            "openai",
            "--endpoint",
            &server.endpoint,
            "--model",
            "formula-boundary-test-model",
            "--no-auto-terms",
            "--no-cache",
            "--layout-replay",
        ])
        .arg(layout_recording_path("unit-form-09-formula-boundary"))
        .arg("--debug")
        .arg(&debug)
        .arg("--output")
        .arg(&output_path)
        .arg(fixture_path("unit-form-09-formula-boundary"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let mut requests = server.wait_for_requests(9);
    requests.sort();
    requests.dedup();
    assert_eq!(
        requests,
        [
            "Heads use {v1}, then continue.",
            "Scale by √{v1}, then continue.",
            "Sequence {v1} is preserved.",
            "We use {v1} during training.",
            "Width is {v1}, then continue.",
        ]
    );

    let read_il = |stage: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(debug.join(stage)).unwrap()).unwrap()
    };
    let before = read_il("03-paragraph_find.il.json");
    let prepared = read_il("04-styles_and_formulas.il.json");
    let translated_snapshot = read_il("06-translate.il.json");
    let after = read_il("09-write.il.json");
    let completed = ["𝑑model=512", "𝜀ls=0.1[36]", "(𝑥1,…,𝑥𝑛)", "ℎ=64", "𝑑model"];
    for (paragraph_index, expected) in completed.into_iter().enumerate() {
        let prepared_chars = prepared["pages"][0]["paragraphs"][paragraph_index]["text"]["chars"]
            .as_array()
            .unwrap();
        let formula = prepared_chars
            .iter()
            .filter(|character| character["layout"]["label"] == "inline_formula")
            .filter_map(|character| character["unicode"].as_str())
            .collect::<String>();
        assert_eq!(formula, expected);

        let before_chars = before["pages"][0]["paragraphs"][paragraph_index]["text"]["chars"]
            .as_array()
            .unwrap();
        let after_chars = after["pages"][0]["paragraphs"][paragraph_index]["text"]["chars"]
            .as_array()
            .unwrap();
        for index in prepared_chars
            .iter()
            .enumerate()
            .filter_map(|(index, character)| {
                (character["layout"]["label"] == "inline_formula").then_some(index)
            })
        {
            assert_eq!(
                after_chars[index]["unicode"],
                before_chars[index]["unicode"]
            );
            assert_eq!(
                after_chars[index]["passthrough"],
                before_chars[index]["passthrough"]
            );
            assert_eq!(after_chars[index]["font"], before_chars[index]["font"]);
            assert_eq!(
                after_chars[index]["font_size"],
                before_chars[index]["font_size"]
            );
            assert_eq!(
                after_chars[index]["baseline_origin"],
                before_chars[index]["baseline_origin"]
            );
            assert_eq!(after_chars[index]["box"], before_chars[index]["box"]);
            assert_eq!(
                after_chars[index]["visual_bbox"],
                before_chars[index]["visual_bbox"]
            );
        }
    }

    let events = parse_events(&output.stdout);
    let evidence = events
        .iter()
        .filter(|event| event["event"] == "diagnostic")
        .filter_map(|event| event["evidence"].as_str())
        .collect::<BTreeSet<_>>();
    assert!(evidence.contains("script_baseline"));
    assert!(evidence.contains("delimiter_completion"));
    assert!(evidence.contains("contiguous_digit_run"));

    let translated = translated_snapshot["pages"][0]["paragraphs"][4]["translated_text"]
        .as_str()
        .unwrap();
    assert_eq!(translated, "A√B.");
    let final_paragraph = &after["pages"][0]["paragraphs"][4];
    // The source root rule shares an unsafe graphics-state scope. The short fake translation
    // exceeds the source-derived fixed-slot continuity limit, so relocation must fail closed.
    assert_eq!(final_paragraph["preserved"], "typeset_protocol");
    assert!(final_paragraph["translated_text"].is_null());
    for extractor in ["poppler", "mupdf"] {
        let extracted = match extractor {
            "poppler" => Command::new("pdftotext")
                .arg(&output_path)
                .arg("-")
                .output()
                .unwrap(),
            "mupdf" => Command::new("mutool")
                .args(["draw", "-F", "txt"])
                .arg(&output_path)
                .output()
                .unwrap(),
            _ => unreachable!(),
        };
        assert!(extracted.status.success(), "{extractor}");
        let compact = String::from_utf8_lossy(&extracted.stdout)
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(compact.contains("√𝑑model"), "{extractor}: {compact:?}");
        assert!(!compact.contains("A√B"), "{extractor}: {compact:?}");
    }
}

#[test]
fn tall_summation_ink_has_one_model_formula_owner_and_one_placeholder() {
    let id = "unit-layout-04-large-summation";
    let inspected = run_inspect_with_layout(id);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraph = &events.last().unwrap()["il"]["pages"][0]["paragraphs"][0];
    let formulas = paragraph["text"]["chars"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|character| character["layout"]["label"] == "inline_formula")
        .collect::<Vec<_>>();
    assert_eq!(formulas.len(), 1, "{paragraph:#?}");
    let summation = formulas[0];
    assert_eq!(summation["unicode"], "∑");
    assert_eq!(summation["layout"]["source"], "model");
    assert_eq!(summation["layout"]["policy"], "passthrough");
    let metric_height =
        summation["box"]["top"].as_f64().unwrap() - summation["box"]["bottom"].as_f64().unwrap();
    let visual_height = summation["visual_bbox"]["top"].as_f64().unwrap()
        - summation["visual_bbox"]["bottom"].as_f64().unwrap();
    assert!(
        visual_height > metric_height * 2.0,
        "summation visual height {visual_height} is not over twice metric height {metric_height}"
    );

    let directory = tempfile::tempdir().unwrap();
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(
        id,
        &server,
        &directory.path().join("translated.pdf"),
        &directory.path().join("debug"),
    );
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.wait_for_requests(1), ["MIMUS {v1} MIMUS"]);
    assert_eq!(server.requests()[0].matches("{v1}").count(), 1);
}

#[test]
fn mixed_source_descents_emit_primary_and_fallback_runs_on_one_baseline() {
    let id = "unit-type-11-mixed-descents";
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &debug);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.wait_for_requests(1), ["MIMUS CIMUS"]);

    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(debug.join("09-write.il.json")).unwrap()).unwrap();
    let paragraph = &written["pages"][0]["paragraphs"][0];
    assert!(paragraph["preserved"].is_null(), "{paragraph:#?}");
    assert_eq!(paragraph["translated_text"], "模型ϵM");
    let content = decoded_page_streams(&output_path, 1).concat();
    assert!(
        content
            .windows(b"/MimusR".len())
            .any(|part| part == b"/MimusR"),
        "{}",
        String::from_utf8_lossy(&content)
    );
    assert!(
        content
            .windows(b"/MimusFR".len())
            .any(|part| part == b"/MimusFR"),
        "{}",
        String::from_utf8_lossy(&content)
    );

    let extracted = Command::new("mutool")
        .args(["draw", "-F", "stext", "-o", "-"])
        .arg(&output_path)
        .arg("1")
        .output()
        .unwrap();
    assert!(
        extracted.status.success(),
        "{}",
        String::from_utf8_lossy(&extracted.stderr)
    );
    let attributes = element_attributes(&extracted.stdout, b"char");
    let text = attributes
        .iter()
        .filter_map(|attributes| attributes.get("c"))
        .map(String::as_str)
        .collect::<String>();
    assert_eq!(text, "模型ϵM");
    let baselines = attributes
        .iter()
        .map(|attributes| number(attributes, "y"))
        .collect::<Vec<_>>();
    let minimum = baselines.iter().copied().reduce(f64::min).unwrap();
    let maximum = baselines.iter().copied().reduce(f64::max).unwrap();
    assert!(
        maximum - minimum < 0.3,
        "output baseline spread is {}pt: {baselines:?}",
        maximum - minimum
    );
}

#[test]
fn model_formula_spans_keep_one_placeholder_per_recorded_region() {
    let id = "unit-form-07-model-spans";
    let directory = tempfile::tempdir().unwrap();
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(
        id,
        &server,
        &directory.path().join("translated.pdf"),
        &debug,
    );
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        server.wait_for_requests(1),
        ["For all {v1} in {v2}, the model agrees."]
    );

    let styles: serde_json::Value = serde_json::from_slice(
        &std::fs::read(debug.join("04-styles_and_formulas.il.json")).unwrap(),
    )
    .unwrap();
    let paragraph = &styles["pages"][0]["paragraphs"][0];
    let mut formulas = BTreeMap::<u64, String>::new();
    for character in paragraph["text"]["chars"].as_array().unwrap() {
        if character["layout"]["label"] != "inline_formula" {
            continue;
        }
        formulas
            .entry(character["layout"]["reading_order"].as_u64().unwrap())
            .or_default()
            .push_str(character["unicode"].as_str().unwrap());
    }
    assert_eq!(
        formulas.into_values().collect::<Vec<_>>(),
        ["x, y", "(a, b)"]
    );
}

#[test]
fn one_model_abstract_region_preserves_cross_column_reading_order() {
    let id = "unit-order-04-model-cross-column";
    let manifest = fixture_manifest(id);
    let expected = manifest
        .expected
        .block
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let inspected = run_inspect_with_layout(id);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 1, "{paragraphs:#?}");
    assert_eq!(il_paragraph_text(&paragraphs[0]), expected);

    let directory = tempfile::tempdir().unwrap();
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(
        id,
        &server,
        &directory.path().join("translated.pdf"),
        &directory.path().join("debug"),
    );
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.wait_for_requests(1), [expected]);
}

#[test]
fn adjacent_han_and_latin_runs_receive_zero_automatic_spacing() {
    let id = "unit-type-06-zero-inter-script-spacing";
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &directory.path().join("debug"));
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        server.wait_for_requests(1),
        ["Inter-script spacing stays explicit."]
    );

    let attributes = mupdf_character_attributes(&output_path);
    assert_eq!(
        attributes
            .iter()
            .map(|character| character["c"].as_str())
            .collect::<String>(),
        "模B模"
    );
    let font_attributes = mupdf_element_attributes(&output_path, b"font");
    assert_eq!(font_attributes.len(), 1, "{font_attributes:#?}");
    let font_size = number(&font_attributes[0], "size");
    let font_bytes = std::fs::read(test_font_path("Regular")).unwrap();
    let face = ttf_parser::Face::parse(&font_bytes, 0).unwrap();
    let expected_advance = |character| {
        let glyph = face.glyph_index(character).unwrap();
        let advance = u64::from(face.glyph_hor_advance(glyph).unwrap());
        let units = u64::from(face.units_per_em());
        ((advance * 1000 + units / 2) / units) as f64 / 1000.0 * font_size
    };
    let origins = attributes
        .iter()
        .map(|character| number(character, "x"))
        .collect::<Vec<_>>();
    assert!((origins[1] - origins[0] - expected_advance('模')).abs() < 0.01);
    assert!((origins[2] - origins[1] - expected_advance('B')).abs() < 0.01);
}

#[test]
fn chinese_kinsoku_rewraps_without_hanging_punctuation() {
    let id = "unit-type-05-cjk-kinsoku";
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &debug);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.wait_for_requests(1), ["MIMUS I"]);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(debug.join("09-write.il.json")).unwrap()).unwrap();
    let paragraph = &written["pages"][0]["paragraphs"][0];
    assert!(paragraph["preserved"].is_null(), "{paragraph:#?}");
    assert_eq!(paragraph["translated_text"], "甲乙丙丁戊（）己庚。");

    let attributes = mupdf_character_attributes(&output_path);
    let mut lines = Vec::<Vec<&BTreeMap<String, String>>>::new();
    for character in &attributes {
        let y = number(character, "y");
        if lines
            .last()
            .is_none_or(|line| (number(line[0], "y") - y).abs() > 0.01)
        {
            lines.push(Vec::new());
        }
        lines.last_mut().unwrap().push(character);
    }
    let texts = lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|character| character["c"].as_str())
                .collect()
        })
        .collect::<Vec<String>>();
    assert_eq!(texts, ["甲乙丙丁戊", "（）己庚。"]);
    for line in lines {
        let first = line.first().unwrap()["c"].chars().next().unwrap();
        let last = line.last().unwrap()["c"].chars().next().unwrap();
        assert!(!mimus_quality_contract::forbidden_line_start(first));
        assert!(!mimus_quality_contract::forbidden_line_end(last));
        assert!((number(line[0], "x") - 72.0).abs() <= 0.01);
        assert!(number(line.last().unwrap(), "x") + 12.0 <= 120.01);
    }
}

#[test]
fn vertical_conflict_preserves_the_later_paragraph_without_moving_it() {
    let id = "unit-type-07-vertical-conflict";
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &debug);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let mut requests = server.wait_for_requests(3);
    requests.sort();
    assert_eq!(
        requests,
        [
            "First control paragraph.",
            "Second conflict paragraph.",
            "Third control paragraph."
        ]
    );
    let events = parse_events(&output.stdout);
    assert!(
        events.iter().any(|event| {
            event["id"] == "typeset_overflow_detail"
                && event["page_index"] == 0
                && event["paragraph_index"] == 1
                && event["obstacle_count"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
        }),
        "{events:#?}"
    );

    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(debug.join("09-write.il.json")).unwrap()).unwrap();
    let paragraphs = written["pages"][0]["paragraphs"].as_array().unwrap();
    assert_eq!(paragraphs[0]["translated_text"], "模型");
    assert!(paragraphs[0]["preserved"].is_null());
    assert!(paragraphs[1]["translated_text"].is_null());
    assert_eq!(paragraphs[1]["preserved"], "typeset_overflow");
    assert_eq!(paragraphs[2]["translated_text"], "数据");
    assert!(paragraphs[2]["preserved"].is_null());

    let input_origins =
        mupdf_character_origins_for_text(&fixture_path(id), "Second conflict paragraph.");
    let output_origins =
        mupdf_character_origins_for_text(&output_path, "Second conflict paragraph.");
    assert_eq!(input_origins.len(), output_origins.len());
    for (input, output) in input_origins.iter().zip(output_origins) {
        assert!(
            (input.0 - output.0).abs() < 0.001,
            "{input:?} != {output:?}"
        );
        assert!(
            (input.1 - output.1).abs() < 0.001,
            "{input:?} != {output:?}"
        );
    }
}

#[test]
fn pure_formula_model_paragraph_is_typed_passthrough_without_a_request() {
    let id = "unit-type-09-model-formula-only";
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &debug);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(server.requests().is_empty());
    let events = parse_events(&output.stdout);
    let passthrough = events
        .iter()
        .filter(|event| event["id"] == "math_passthrough")
        .collect::<Vec<_>>();
    assert_eq!(passthrough.len(), 1, "{events:#?}");
    assert_eq!(passthrough[0]["page_index"], 0);
    assert_eq!(passthrough[0]["paragraph_index"], 0);
    assert_eq!(passthrough[0]["source_characters"], 9);
    let input_path = fixture_path(id);
    assert_eq!(
        decoded_page_streams(&output_path, 1),
        decoded_page_streams(&input_path, 1)
    );
    assert_eq!(
        page_font_resource_names(&output_path, 1),
        page_font_resource_names(&input_path, 1)
    );
    assert!(
        std::fs::read(&output_path)
            .unwrap()
            .starts_with(&std::fs::read(input_path).unwrap())
    );
}

#[test]
fn natural_paragraph_split_creates_exactly_two_requests_from_dual_evidence() {
    let id = "unit-para-05-natural-split";
    let inspected = run_inspect_with_layout(id);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 2, "{paragraphs:#?}");
    assert_eq!(
        paragraphs.iter().map(il_paragraph_text).collect::<Vec<_>>(),
        ["M M", "M M"]
    );
    assert_eq!(paragraphs[0].get("first_line_indent"), None);
    assert_eq!(paragraphs[1]["first_line_indent"], 24.0);

    let directory = tempfile::tempdir().unwrap();
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(
        id,
        &server,
        &directory.path().join("translated.pdf"),
        &directory.path().join("debug"),
    );
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.wait_for_requests(2), ["M M", "M M"]);
}

#[test]
fn source_indents_are_inferred_and_preserved_as_absolute_output_deltas() {
    let id = "unit-para-09-indent-preservation";
    let inspected = run_inspect_with_layout(id);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 3, "{paragraphs:#?}");
    assert_eq!(paragraphs[0].get("first_line_indent"), None);
    assert_eq!(paragraphs[1]["first_line_indent"], 12.0);
    assert_eq!(paragraphs[2]["first_line_indent"], 36.0);
    assert!(paragraphs.iter().all(|paragraph| {
        paragraph["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| character["layout"]["policy"] == "translate")
    }));

    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &directory.path().join("debug"));
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.wait_for_requests(3), ["M M M", "M M M", "M M M"]);

    let mut line_starts = Vec::<(f64, f64)>::new();
    for character in mupdf_character_attributes(&output_path) {
        let x = number(&character, "x");
        let y = number(&character, "y");
        if let Some((_, start)) = line_starts
            .iter_mut()
            .find(|(baseline, _)| (*baseline - y).abs() <= 0.01)
        {
            *start = start.min(x);
        } else {
            line_starts.push((y, x));
        }
    }
    line_starts.sort_by(|left, right| left.0.total_cmp(&right.0));
    assert_eq!(line_starts.len(), 4, "{line_starts:?}");
    for ((_, start), expected_indent) in line_starts[..3].iter().zip([0.0, 12.0, 36.0]) {
        assert_close(*start - 60.0, expected_indent, 0.01);
    }
    assert_close(line_starts[3].1, 60.0, 0.01);
}

#[test]
fn translated_body_font_size_uses_the_non_script_character_mode() {
    let id = "unit-form-14-font-size-mode";
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &directory.path().join("debug"));
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.wait_for_requests(1), ["MIIMIIMIIM"]);

    let fonts = mupdf_element_attributes(&output_path, b"font");
    assert_eq!(fonts.len(), 1, "{fonts:#?}");
    assert_close(number(&fonts[0], "size"), 10.0, 0.001);
}

#[test]
fn form_comma_and_numeric_prose_requests_keep_literal_punctuation() {
    let directory = tempfile::tempdir().unwrap();
    let server = FakeResponsesServer::start();
    for id in ["unit-form-03-comma-ownership", "unit-form-06-numeric-prose"] {
        let request_count = server.requests().len();
        let debug = directory.path().join(format!("{id}-debug"));
        let output = run_openai_with_layout(
            id,
            &server,
            &directory.path().join(format!("{id}.pdf")),
            &debug,
        );
        assert!(
            output.status.success(),
            "fixture {id}\nstderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        assert_request_count_after_fixture(id, &server, request_count + 1, &output, &debug);
    }

    assert_eq!(
        server.wait_for_requests(2),
        [
            "The result {v1} = 1, 2, 3 shows the distinction.",
            "In 2024, we measured 3.14 and 1, 2, 3.",
        ]
    );
    assert_eq!(server.requests()[0].matches("{v").count(), 1);
    assert_eq!(server.requests()[1].matches("{v").count(), 0);
}

#[test]
fn form_script_placeholders_are_invariant_to_a_leading_bullet() {
    let directory = tempfile::tempdir().unwrap();
    let server = FakeResponsesServer::start();
    for id in [
        "unit-form-05-scripts-control",
        "unit-form-05-scripts-bullet",
    ] {
        let request_count = server.requests().len();
        let debug = directory.path().join(format!("{id}-debug"));
        let output = run_openai_with_layout(
            id,
            &server,
            &directory.path().join(format!("{id}.pdf")),
            &debug,
        );
        assert!(
            output.status.success(),
            "fixture {id}\nstderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        assert_request_count_after_fixture(id, &server, request_count + 1, &output, &debug);
    }

    let requests = server.wait_for_requests(2);
    assert_eq!(requests[0], "Compare {v1} with the reference.");
    assert_eq!(requests[1], "• Compare {v1} with the reference.");
    assert_eq!(requests[0].matches("{v").count(), 1);
    assert_eq!(requests[1].matches("{v").count(), 1);
    assert_eq!(requests[1].strip_prefix("• "), Some(requests[0].as_str()));
}

#[test]
fn form_bold_protocol_has_no_historical_placeholder_cutoff() {
    let directory = tempfile::tempdir().unwrap();
    let server = FakeResponsesServer::start();
    for id in ["unit-form-10-mixed-styles", "unit-form-10-bold-stress"] {
        let request_count = server.requests().len();
        let output_path = directory.path().join(format!("{id}.pdf"));
        let debug = directory.path().join(format!("{id}-debug"));
        let output = run_openai_with_layout(id, &server, &output_path, &debug);
        assert!(
            output.status.success(),
            "fixture {id}\nstderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        assert_request_count_after_fixture(id, &server, request_count + 1, &output, &debug);
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(debug.join("09-write.il.json")).unwrap())
                .unwrap();
        let preserved = &written["pages"][0]["paragraphs"][0]["preserved"];
        assert!(
            preserved.is_null(),
            "fixture {id} preserved as {preserved}; requests={:?}\nstderr: {}\nstdout: {}",
            server.requests(),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        );
    }

    let requests = server.wait_for_requests(2);
    assert_eq!(
        requests[0],
        "Regular <b1>strong</b1> emphasis (small note) x2 end."
    );
    assert_eq!(requests[0].matches("<b").count(), 1);
    assert_eq!(requests[0].matches("</b").count(), 1);
    assert_eq!(requests[1].matches("<b").count(), 45);
    assert_eq!(requests[1].matches("</b").count(), 45);

    let extracted = Command::new("pdftotext")
        .arg(directory.path().join("unit-form-10-bold-stress.pdf"))
        .arg("-")
        .output()
        .unwrap();
    assert!(extracted.status.success());
    assert_eq!(
        String::from_utf8_lossy(&extracted.stdout)
            .matches('粗')
            .count(),
        45
    );
}

#[test]
fn form_fraction_rule_and_glyphs_relocate_by_the_same_delta() {
    let id = "unit-form-11-fraction-rule";
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &debug);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_request_count_after_fixture(id, &server, 1, &output, &debug);
    assert_eq!(
        server.wait_for_requests(1),
        ["The ratio{v1} remains attached while this narrow paragraph wraps onto a second line."]
    );

    let read_il = |stage: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(debug.join(stage)).unwrap()).unwrap()
    };
    let before = read_il("04-styles_and_formulas.il.json");
    let after = read_il("09-write.il.json");
    assert!(
        after["pages"][0]["paragraphs"][0]["preserved"].is_null(),
        "fraction paragraph failed closed as {:?}",
        after["pages"][0]["paragraphs"][0]["preserved"]
    );
    let source_formula = before["pages"][0]["paragraphs"][0]["text"]["chars"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|character| character["layout"]["label"] == "inline_formula")
        .filter_map(|character| character["unicode"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(source_formula, ["a", "b"]);

    let trace = |path: &Path| {
        let output = Command::new("mutool")
            .args(["draw", "-F", "trace"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    let formula_transforms = |xml: &[u8]| {
        let mut reader = Reader::from_reader(xml);
        let mut current_transform = None;
        let mut transforms = BTreeMap::new();
        loop {
            match reader.read_event().unwrap() {
                Event::Start(element) if element.local_name().as_ref() == b"fill_text" => {
                    current_transform = element.attributes().find_map(|attribute| {
                        let attribute = attribute.unwrap();
                        (attribute.key.local_name().as_ref() == b"transform").then(|| {
                            attribute
                                .normalized_value(XmlVersion::Implicit1_0)
                                .unwrap()
                                .split_ascii_whitespace()
                                .map(|value| value.parse::<f64>().unwrap())
                                .collect::<Vec<_>>()
                        })
                    });
                }
                Event::Empty(element)
                    if element.local_name().as_ref() == b"g" && current_transform.is_some() =>
                {
                    let unicode = element.attributes().find_map(|attribute| {
                        let attribute = attribute.unwrap();
                        (attribute.key.local_name().as_ref() == b"unicode").then(|| {
                            attribute
                                .normalized_value(XmlVersion::Implicit1_0)
                                .unwrap()
                                .into_owned()
                        })
                    });
                    if let Some(unicode @ ("a" | "b")) = unicode.as_deref() {
                        transforms.insert(unicode.to_owned(), current_transform.clone().unwrap());
                    }
                }
                Event::End(element) if element.local_name().as_ref() == b"fill_text" => {
                    current_transform = None;
                }
                Event::Eof => break,
                _ => {}
            }
        }
        transforms
    };
    let stroke_transform = |xml: &[u8]| {
        first_element_attributes(xml, b"stroke_path")["transform"]
            .split_ascii_whitespace()
            .map(|value| value.parse::<f64>().unwrap())
            .collect::<Vec<_>>()
    };
    let source_trace = trace(&fixture_path(id));
    let output_trace = trace(&output_path);
    let source_glyphs = formula_transforms(&source_trace);
    let output_glyphs = formula_transforms(&output_trace);
    assert_eq!(source_glyphs.keys().collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(
        source_glyphs.keys().collect::<Vec<_>>(),
        output_glyphs.keys().collect::<Vec<_>>()
    );
    let delta_x = output_glyphs["a"][4] - source_glyphs["a"][4];
    let delta_y = output_glyphs["a"][5] - source_glyphs["a"][5];
    assert!(
        delta_x.abs() > 0.01 || delta_y.abs() > 0.01,
        "fraction stayed fixed: source={source_glyphs:?}, output={output_glyphs:?}"
    );
    for unicode in ["a", "b"] {
        assert!((output_glyphs[unicode][4] - source_glyphs[unicode][4] - delta_x).abs() <= 0.01);
        assert!((output_glyphs[unicode][5] - source_glyphs[unicode][5] - delta_y).abs() <= 0.01);
    }
    let source_stroke = stroke_transform(&source_trace);
    let output_stroke = stroke_transform(&output_trace);
    assert_eq!(source_stroke.len(), 6);
    assert_eq!(output_stroke.len(), 6);
    assert!((output_stroke[4] - source_stroke[4] - delta_x).abs() <= 0.05);
    assert!((output_stroke[5] - source_stroke[5] - delta_y).abs() <= 0.05);
}

#[test]
fn uniquely_owned_text_underline_replays_with_its_replacement_delta() {
    let id = "unit-form-12-text-underline";
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &debug);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(server.wait_for_requests(1), ["MIMUS MIMUS"]);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(debug.join("09-write.il.json")).unwrap()).unwrap();
    let paragraph = &written["pages"][0]["paragraphs"][0];
    assert!(paragraph["preserved"].is_null(), "{paragraph:#?}");
    assert_eq!(paragraph["translated_text"], "M");

    let content = decoded_page_streams(&output_path, 1).concat();
    let content = String::from_utf8(content).unwrap();
    assert!(content.contains("1 0 0 1 -18 0 cm\n"), "{content}");
    assert_eq!(content.matches("90 138 m").count(), 1, "{content}");

    let trace = Command::new("mutool")
        .args(["draw", "-F", "trace"])
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    let strokes = element_attributes(&trace.stdout, b"stroke_path");
    assert_eq!(
        strokes.len(),
        1,
        "{}",
        String::from_utf8_lossy(&trace.stdout)
    );
    let transform = strokes[0]["transform"]
        .split_ascii_whitespace()
        .map(|value| value.parse::<f64>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(transform.len(), 6);
    assert!((transform[4] + 18.0).abs() <= 0.001, "{transform:?}");
    assert!((transform[5] - 200.0).abs() <= 0.001, "{transform:?}");
}

#[test]
fn para_small_edge_character_keeps_unique_model_ownership() {
    let id = "unit-para-02-edge-superscript";
    let output = run_inspect_with_layout(id);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 1, "{paragraphs:#?}");
    assert_eq!(il_paragraph_text(&paragraphs[0]), "Boundary marker word¹");

    let chars = paragraphs[0]["text"]["chars"].as_array().unwrap();
    let marker = chars
        .iter()
        .find(|character| character["unicode"] == "¹")
        .unwrap();
    assert_eq!(marker["font_size"], 5.0);
    assert_eq!(marker["layout"]["source"], "model");
    assert_eq!(marker["layout"]["label"], "text");
    let edge_crossing = marker["box"]["right"].as_f64().unwrap()
        - marker["layout"]["bounds"]["right"].as_f64().unwrap();
    assert!((edge_crossing - 1.0).abs() <= 0.001, "{edge_crossing}");

    let rect_area = |value: &serde_json::Value| {
        (value["right"].as_f64().unwrap() - value["left"].as_f64().unwrap())
            * (value["top"].as_f64().unwrap() - value["bottom"].as_f64().unwrap())
    };
    let mut body_metric_areas = chars
        .iter()
        .filter(|character| {
            character["unicode"] != "¹"
                && character["unicode"]
                    .as_str()
                    .is_some_and(|unicode| !unicode.trim().is_empty())
        })
        .map(|character| rect_area(&character["box"]))
        .collect::<Vec<_>>();
    body_metric_areas.sort_by(f64::total_cmp);
    let median_metric_area = body_metric_areas[body_metric_areas.len() / 2];
    let marker_visual_area = rect_area(&marker["visual_bbox"]);
    assert!(
        marker_visual_area < median_metric_area * 0.05,
        "marker visual area {marker_visual_area} is not below 5% of median body metric area {median_metric_area}"
    );
}

#[test]
fn para_bullet_controls_keep_only_real_list_boundaries() {
    let expected = [
        (
            "unit-para-06-real-bullet",
            vec!["• First item.", "• Second item."],
        ),
        (
            "unit-para-06-leading-middle-dot",
            vec!["· Emphasis begins this ordinary sentence."],
        ),
        (
            "unit-para-06-inline-superscript",
            vec!["Footnote marker appears inside word¹ without starting a new paragraph."],
        ),
    ];
    for (id, expected_paragraphs) in &expected {
        let output = run_inspect_with_layout(id);
        assert!(
            output.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = parse_events(&output.stdout);
        let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
            .as_array()
            .unwrap();
        assert_eq!(
            paragraphs.iter().map(il_paragraph_text).collect::<Vec<_>>(),
            *expected_paragraphs,
            "fixture {id}"
        );
    }

    let directory = tempfile::tempdir().unwrap();
    let server = FakeResponsesServer::start();
    let mut expected_requests = Vec::new();
    for (id, paragraphs) in expected {
        let output = run_openai_with_layout(
            id,
            &server,
            &directory.path().join(format!("{id}.pdf")),
            &directory.path().join(format!("{id}-debug")),
        );
        assert!(
            output.status.success(),
            "fixture {id}\nstderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        expected_requests.extend(paragraphs.into_iter().map(str::to_owned));
        let mut actual_requests = server.wait_for_requests(expected_requests.len());
        actual_requests.sort();
        expected_requests.sort();
        assert_eq!(actual_requests, expected_requests, "fixture {id}");
    }
}

#[test]
fn para_overlapping_model_boxes_produce_contained_nonoverlapping_paragraphs() {
    let id = "unit-para-08-overlapping-model-boxes";
    let output = run_inspect_with_layout(id);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let events = parse_events(&output.stdout);
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 2, "{paragraphs:#?}");
    assert_eq!(
        il_paragraph_text(&paragraphs[0]),
        "Upper paragraph ends beside ∑."
    );
    assert_eq!(
        il_paragraph_text(&paragraphs[1]),
        "Lower paragraph remains separate and fully contained."
    );
    assert!(
        paragraphs[0]["bounds"]["bottom"].as_f64().unwrap()
            >= paragraphs[1]["bounds"]["top"].as_f64().unwrap(),
        "paragraph bounds overlap: {paragraphs:#?}"
    );
    for paragraph in paragraphs {
        let bounds = &paragraph["bounds"];
        for character in paragraph["text"]["chars"].as_array().unwrap() {
            let visual = &character["visual_bbox"];
            assert!(
                visual["left"].as_f64().unwrap() >= bounds["left"].as_f64().unwrap() - 0.001
                    && visual["bottom"].as_f64().unwrap()
                        >= bounds["bottom"].as_f64().unwrap() - 0.001
                    && visual["right"].as_f64().unwrap()
                        <= bounds["right"].as_f64().unwrap() + 0.001
                    && visual["top"].as_f64().unwrap() <= bounds["top"].as_f64().unwrap() + 0.001,
                "visual box escaped paragraph bounds: {character:#?} in {bounds:#?}"
            );
        }
    }
}

#[test]
fn para_narrow_columns_reconstruct_exact_requests_from_inferred_line_spaces() {
    let id = "unit-para-11-narrow-columns";
    let manifest = fixture_manifest(id);
    let expected = manifest
        .expected
        .block
        .iter()
        .map(|block| block.text.clone())
        .collect::<Vec<_>>();
    let output = run_inspect_with_layout(id);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let events = parse_events(&output.stdout);
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    assert_eq!(paragraphs.len(), 3, "{paragraphs:#?}");
    assert_eq!(
        paragraphs.iter().map(il_paragraph_text).collect::<Vec<_>>(),
        expected
    );
    for paragraph in paragraphs {
        let chars = paragraph["text"]["chars"].as_array().unwrap();
        let baselines = chars
            .iter()
            .map(|character| {
                character["baseline_origin"]["y"]
                    .as_f64()
                    .unwrap()
                    .to_bits()
            })
            .collect::<BTreeSet<_>>();
        let inferred_spaces = chars
            .iter()
            .filter(|character| character["implicit_space_before"] == true)
            .count();
        assert_eq!(baselines.len(), 10);
        assert_eq!(inferred_spaces, 9);
    }

    let directory = tempfile::tempdir().unwrap();
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(
        id,
        &server,
        &directory.path().join("translated.pdf"),
        &directory.path().join("debug"),
    );
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let mut actual_requests = server.wait_for_requests(3);
    actual_requests.sort();
    let mut expected_requests = expected;
    expected_requests.sort();
    assert_eq!(actual_requests, expected_requests);
}

#[test]
fn para_algorithm_indentation_makes_no_request_and_publishes_source_streams_unchanged() {
    let id = "unit-para-12-algorithm-indentation";
    let input_path = fixture_path(id);
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("translated.pdf");
    let debug = directory.path().join("debug");
    let server = FakeResponsesServer::start();
    let output = run_openai_with_layout(id, &server, &output_path, &debug);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(server.requests().is_empty());

    let paragraph_find: serde_json::Value =
        serde_json::from_slice(&std::fs::read(debug.join("03-paragraph_find.il.json")).unwrap())
            .unwrap();
    let paragraph = &paragraph_find["pages"][0]["paragraphs"][0];
    assert_eq!(
        il_paragraph_text(paragraph),
        "root  child    grandchild      leaf"
    );
    assert!(
        paragraph["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| {
                character["layout"]["label"] == "algorithm"
                    && character["layout"]["policy"] == "passthrough"
            })
    );
    assert_eq!(
        decoded_page_streams(&output_path, 1),
        decoded_page_streams(&input_path, 1)
    );
}

#[test]
fn prose_mislabeled_as_footer_below_a_title_is_recovered_without_touching_the_folio() {
    let output = run_inspect_with_recording(
        "unit-layout-07-policy-zones",
        "unit-layout-07-false-footer-body",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraphs = events.last().unwrap()["il"]["pages"][0]["paragraphs"]
        .as_array()
        .unwrap();
    let body = paragraphs
        .iter()
        .find(|paragraph| {
            paragraph["text"]["chars"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|character| character["unicode"].as_str())
                .collect::<String>()
                .starts_with("The second body paragraph")
        })
        .unwrap();
    assert!(
        body["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| character["layout"]["label"] == "text"
                && character["layout"]["policy"] == "translate")
    );
    let folio = paragraphs
        .iter()
        .find(|paragraph| {
            paragraph["text"]["chars"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|character| character["unicode"].as_str())
                .collect::<String>()
                == "17"
        })
        .unwrap();
    assert!(
        folio["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| character["layout"]["label"] == "number"
                && character["layout"]["policy"] == "passthrough")
    );
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
    assert!(help.contains("--bilingual"));
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
    assert!(paragraphs.iter().all(|paragraph| {
        if paragraph.get("preserved").is_some() {
            return true;
        }
        let source = il_paragraph_text(paragraph);
        let tokens = mimus_quality_contract::conserved_tokens(&source).join(" ");
        let expected = if tokens.is_empty() {
            "M".to_owned()
        } else {
            format!("M{tokens}")
        };
        paragraph["translated_text"] == expected
    }));
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
        "unit-font-04-negative-descent-parent",
        "mal-font-04-positive-descent",
        "unit-font-08-type1-header-encoding",
        "unit-font-10-estimated-bbox",
        "unit-font-escaped-name",
        "unit-stream-02-type3-d1",
        "unit-stream-04-type3-d0",
        "unit-cmap-01-identity-no-tounicode",
        "unit-cmap-02-mixed-codespace",
        "unit-cmap-embedded-ok",
        "unit-cmap-identity-alias",
        "unit-cmap-09-valid-scalar-parent",
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
fn font_10_estimated_bbox_is_conservative_typed_and_translatable() {
    let id = "unit-font-10-estimated-bbox";
    let inspected = run_inspect(&fixture_path(id), true, None);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let estimated = events
        .iter()
        .filter(|event| event["id"] == "glyph_bbox_estimated")
        .collect::<Vec<_>>();
    assert_eq!(estimated.len(), 1, "{events:#?}");
    assert_eq!(estimated[0]["page_index"], 0);
    assert_eq!(estimated[0]["character_index"], 0);
    assert_eq!(estimated[0]["font_object"], serde_json::json!([5, 0]));
    assert_eq!(estimated[0]["code"], 65_535);

    let paragraph = &events.last().unwrap()["il"]["pages"][0]["paragraphs"][0];
    assert!(paragraph.get("preserved").is_none(), "{paragraph:#?}");
    let character = &paragraph["text"]["chars"][0];
    assert_eq!(character["unicode"], "M");
    assert_eq!(character["bbox_estimated"], true);
    assert_close(
        character["box"]["right"].as_f64().unwrap() - character["box"]["left"].as_f64().unwrap(),
        7.2,
        0.001,
    );
    for (estimated_edge, metric_edge, relation) in [
        ("left", "left", std::cmp::Ordering::Less),
        ("bottom", "bottom", std::cmp::Ordering::Less),
        ("right", "right", std::cmp::Ordering::Greater),
        ("top", "top", std::cmp::Ordering::Greater),
    ] {
        assert_eq!(
            character["visual_bbox"][estimated_edge]
                .as_f64()
                .unwrap()
                .total_cmp(&character["box"][metric_edge].as_f64().unwrap()),
            relation,
            "{estimated_edge}: {character:#?}"
        );
    }
}

#[test]
fn font_04_positive_descent_is_typed_and_normalized_in_production_il() {
    let inspected = run_inspect(&fixture_path("mal-font-04-positive-descent"), true, None);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    assert!(events.iter().any(|event| {
        event["event"] == "diagnostic"
            && event["id"] == "content_recovered"
            && event["recovery"] == "normalized_font_descent"
    }));
    let character = &events.last().unwrap()["il"]["pages"][0]["paragraphs"][0]["text"]["chars"][0];
    assert!((character["box"]["bottom"].as_f64().unwrap() - 117.48).abs() <= 0.001);

    let parent = run_inspect(
        &fixture_path("unit-font-04-negative-descent-parent"),
        true,
        None,
    );
    let parent_events = parse_events(&parent.stdout);
    assert!(parent_events.iter().all(|event| {
        event["id"] != "content_recovered" || event["recovery"] != "normalized_font_descent"
    }));
}

#[test]
fn explicit_differences_agl_single_scalars_are_auditable_and_translatable() {
    let directory = tempfile::tempdir().unwrap();
    for id in [
        "unit-cmap-10-differences-agl-type1",
        "unit-cmap-11-differences-agl-type3",
    ] {
        let input = fixture_path(id);
        let inspected = run_inspect(&input, true, None);
        assert!(
            inspected.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        let events = parse_events(&inspected.stdout);
        assert_one_terminal_last(&events, "result");
        let paragraph = &events.last().unwrap()["il"]["pages"][0]["paragraphs"][0];
        assert!(
            paragraph.get("preserved").is_none(),
            "fixture {id} was preserved: {paragraph}"
        );
        assert_eq!(paragraph["text"]["chars"][0]["unicode"], "Á", "{id}");
        assert_eq!(
            paragraph["text"]["chars"][0]["unicode_source"], "differences_agl",
            "{id}"
        );
        assert!(
            events.iter().any(|event| {
                event["event"] == "diagnostic"
                    && event["id"] == "unicode_recovered"
                    && event["page_index"] == 0
                    && event["paragraph_index"] == 0
                    && event["reading_order"] == 0
                    && event["recovered_character_count"] == 1
            }),
            "fixture {id} has no typed recovery diagnostic: {events:?}"
        );

        let output = directory.path().join(format!("{id}.pdf"));
        let debug = directory.path().join(format!("{id}-debug"));
        let translated = run_none_with_debug(&input, &output, &debug, true);
        assert!(
            translated.status.success(),
            "fixture {id}: {}",
            String::from_utf8_lossy(&translated.stderr)
        );
        let il: serde_json::Value =
            serde_json::from_slice(&std::fs::read(debug.join("06-translate.il.json")).unwrap())
                .unwrap();
        let paragraph = &il["pages"][0]["paragraphs"][0];
        assert_eq!(paragraph["translated_text"], "Á", "{id}");
        assert!(paragraph.get("preserved").is_none(), "{id}");
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
        "mal-cmap-09-isolated-surrogate",
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
fn isolated_tounicode_surrogate_stays_null_and_preserves_the_paragraph() {
    let id = "mal-cmap-09-isolated-surrogate";
    let input = fixture_path(id);
    let inspected = run_inspect(&input, true, None);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraph = &events.last().unwrap()["il"]["pages"][0]["paragraphs"][0];
    let characters = paragraph["text"]["chars"].as_array().unwrap();
    assert_eq!(characters.len(), 1);
    assert!(characters[0]["unicode"].is_null());
    assert_eq!(paragraph["preserved"], "unreliable_unicode");
    assert!(!String::from_utf8_lossy(&inspected.stdout).contains('\u{fffd}'));
    let summary = events
        .iter()
        .find(|event| event["event"] == "diagnostic" && event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["degraded_page_indices"], serde_json::json!([]));
    assert_eq!(
        summary["preserved_paragraphs"],
        serde_json::json!([{
            "page_index": 0,
            "paragraph_index": 0,
            "reason": "unreliable_unicode",
        }])
    );

    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("translated.pdf");
    let output = run_none(&input, Some(&translated), true);
    assert!(output.status.success());
    assert_eq!(
        std::fs::read(translated).unwrap(),
        std::fs::read(input).unwrap()
    );
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
fn parse_06_reports_the_malformed_object_and_xref_offset_without_output() {
    let id = "mal-parse-06-object-syntax";
    let input = fixture_path(id);
    let bytes = std::fs::read(&input).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("must-not-exist.pdf");
    let output = run_none(&input, Some(&translated), true);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "error");
    let error = events.last().unwrap();
    assert_eq!(error["category"], "input");
    assert_eq!(error["reason"], "pdf_parse");
    assert_eq!(error["detail"]["kind"], "object_syntax");
    assert_eq!(error["detail"]["objid"], serde_json::json!([12, 0]));
    let offset = error["detail"]["offset"].as_u64().unwrap() as usize;
    assert_eq!(offset, 3533);
    assert!(bytes[offset..].starts_with(b"12 0 obj"));
    assert!(!translated.exists());
}

#[test]
fn stream_03_path_arity_degrades_the_page_and_preserves_exact_bytes() {
    let id = "mal-stream-12-path-arity";
    let input = fixture_path(id);
    let inspected = run_inspect(&input, true, None);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    assert_eq!(
        events.last().unwrap()["il"]["pages"][0]["paragraphs"],
        serde_json::json!([])
    );
    let degraded = events
        .iter()
        .filter(|event| event["id"] == "page_degraded")
        .collect::<Vec<_>>();
    assert_eq!(degraded.len(), 1, "{events:#?}");
    assert_eq!(degraded[0]["page_index"], 0);
    assert_eq!(degraded[0]["reason"], "graphics_unreliable");

    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("translated.pdf");
    let output = run_none(&input, Some(&translated), true);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        std::fs::read(&translated).unwrap(),
        std::fs::read(input).unwrap()
    );
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
            "mal-parse-06-object-syntax",
            (2, OutputExpectation::Missing, None),
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
            "mal-stream-12-path-arity",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
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
            "mal-xobj-09-not-a-stream",
            (0, OutputExpectation::Exact, Some("degradation_summary")),
        ),
        (
            "mal-xobj-11-reversed-bbox-form-text",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
        ),
        (
            "mal-xobj-11-reversed-bbox-page-text",
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
            "unit-parse-12-contents-array-tj-operand",
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
            "unit-stream-12-path-parent",
            (0, OutputExpectation::Rewritten, None),
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
            "unit-xobj-09-stream-parent",
            (0, OutputExpectation::Exact, None),
        ),
        (
            "unit-xobj-11-bbox-order-parent",
            (0, OutputExpectation::Rewritten, None),
        ),
        (
            "unit-xobj-12-form-bbox-clip",
            (0, OutputExpectation::Rewritten, Some("content_recovered")),
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
            (0, OutputExpectation::Exact, Some("degradation_summary")),
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
fn pdfpages_shaped_form_text_is_typed_but_chart_text_is_not() {
    let id = "unit-xobj-m1-switchboard";
    let input = fixture_path(id);
    let inspected = run_inspect_with_recording(id, id);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let paragraph = &events.last().unwrap()["il"]["pages"][0]["paragraphs"][0];
    assert_eq!(paragraph["preserved"], "form_xobject_content");
    assert!(
        paragraph["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| character["layout"]["policy"] == "translate"
                && character["passthrough"]["content_object"] == 10)
    );
    let summary = events
        .iter()
        .find(|event| event["event"] == "diagnostic" && event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(
        summary["preserved_paragraphs"],
        serde_json::json!([{
            "page_index": 0,
            "paragraph_index": 0,
            "reason": "form_xobject_content",
        }])
    );

    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("form.pdf");
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let output = command
        .env(PDFIUM_ENV, pdfium_library())
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--layout-replay",
        ])
        .arg(layout_recording_path(id))
        .arg("--output")
        .arg(&translated)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(&translated).unwrap(),
        std::fs::read(&input).unwrap()
    );
    let events = parse_events(&output.stdout);
    assert!(events.iter().any(|event| {
        event["event"] == "diagnostic"
            && event["id"] == "degradation_summary"
            && event["preserved_paragraphs"][0]["reason"] == "form_xobject_content"
    }));

    let strict_path = directory.path().join("strict.pdf");
    let mut command = Command::new(BIN);
    configure_test_fonts(&mut command);
    let strict = command
        .env(PDFIUM_ENV, pdfium_library())
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--strict",
            "--layout-replay",
        ])
        .arg(layout_recording_path(id))
        .arg("--output")
        .arg(&strict_path)
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(strict.status.code(), Some(4));
    assert!(!strict_path.exists());
    let strict_events = parse_events(&strict.stdout);
    assert_one_terminal_last(&strict_events, "error");
    assert_eq!(
        strict_events.last().unwrap()["reason"],
        "strict_degradation"
    );

    let chart = run_inspect_with_recording(id, "unit-xobj-m1-switchboard-chart");
    assert!(
        chart.status.success(),
        "{}",
        String::from_utf8_lossy(&chart.stderr)
    );
    let chart_events = parse_events(&chart.stdout);
    assert_one_terminal_last(&chart_events, "result");
    let chart_paragraph = &chart_events.last().unwrap()["il"]["pages"][0]["paragraphs"][0];
    assert!(chart_paragraph.get("preserved").is_none());
    assert!(
        chart_paragraph["text"]["chars"]
            .as_array()
            .unwrap()
            .iter()
            .all(|character| character["layout"]["policy"] == "passthrough")
    );
    assert!(
        !chart_events.iter().any(|event| {
            event["event"] == "diagnostic" && event["id"] == "degradation_summary"
        })
    );
}

#[test]
fn non_stream_xobject_degrades_the_page_and_republishes_exact_bytes() {
    let id = "mal-xobj-09-not-a-stream";
    let input = fixture_path(id);
    let inspected = run_inspect(&input, true, None);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = parse_events(&inspected.stdout);
    assert_one_terminal_last(&events, "result");
    let degraded = events
        .iter()
        .find(|event| event["event"] == "diagnostic" && event["id"] == "page_degraded")
        .unwrap();
    assert_eq!(degraded["page_index"], 0);
    assert_eq!(degraded["reason"], "x_object_not_a_stream");
    assert_eq!(
        events.last().unwrap()["il"]["pages"][0]["paragraphs"],
        serde_json::json!([])
    );
    let summary = events
        .iter()
        .find(|event| event["event"] == "diagnostic" && event["id"] == "degradation_summary")
        .unwrap();
    assert_eq!(summary["degraded_page_indices"], serde_json::json!([0]));

    let directory = tempfile::tempdir().unwrap();
    let translated = directory.path().join("translated.pdf");
    let output = run_none(&input, Some(&translated), true);
    assert!(output.status.success());
    assert_eq!(
        std::fs::read(translated).unwrap(),
        std::fs::read(input).unwrap()
    );
}

#[test]
fn form_bbox_clipped_text_is_reported_and_never_becomes_visible_ink() {
    let id = "unit-xobj-12-form-bbox-clip";
    let output = run_inspect(&fixture_path(id), true, None);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_events(&output.stdout);
    assert_one_terminal_last(&events, "result");
    assert!(events.iter().all(|event| event["id"] != "page_degraded"));

    let clipped = events
        .iter()
        .filter(|event| {
            event["id"] == "content_recovered" && event["recovery"] == "clipped_form_content"
        })
        .collect::<Vec<_>>();
    assert_eq!(clipped.len(), 1, "{events:?}");
    assert_eq!(clipped[0]["page_index"], 1);
    assert_eq!(clipped[0]["form_object_ids"], serde_json::json!([11]));
    assert_eq!(clipped[0]["form_object_count"], 1);
}

#[test]
fn reversed_form_bbox_is_recovered_with_typed_object_location_without_page_degradation() {
    for (id, page_index, form_object) in [
        ("mal-xobj-11-reversed-bbox-page-text", 0, 10),
        ("mal-xobj-11-reversed-bbox-form-text", 1, 13),
    ] {
        let output = run_inspect(&fixture_path(id), true, None);
        assert_eq!(
            output.status.code(),
            Some(0),
            "fixture {id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = parse_events(&output.stdout);
        assert_one_terminal_last(&events, "result");
        assert!(events.iter().all(|event| event["id"] != "page_degraded"));

        let recoveries = events
            .iter()
            .filter(|event| {
                event["id"] == "content_recovered" && event["recovery"] == "normalized_form_bbox"
            })
            .collect::<Vec<_>>();
        assert_eq!(recoveries.len(), 1, "fixture {id}: {events:?}");
        assert_eq!(recoveries[0]["page_index"], page_index);
        assert_eq!(
            recoveries[0]["form_object_ids"],
            serde_json::json!([form_object])
        );
        assert_eq!(recoveries[0]["form_object_count"], 1);
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

#[test]
fn bounded_write_faults_never_publish_partial_output_and_allow_reentry() {
    const EXISTING: &[u8] = b"existing destination";
    const PRE_PERSIST_FAULTS: &[&str] = &[
        "kill_after_temp_create",
        "kill_after_partial_write",
        "oom_after_partial_write",
        "kill_before_persist",
    ];

    let input = fixture();
    let baseline_directory = tempfile::tempdir().unwrap();
    let baseline_path = baseline_directory.path().join("translated.pdf");
    let baseline = run_none(&input, Some(&baseline_path), true);
    assert!(
        baseline.status.success(),
        "could not establish publication baseline: {}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let expected = std::fs::read(&baseline_path).unwrap();
    let input_bytes = std::fs::read(&input).unwrap();
    assert!(expected.starts_with(&input_bytes));
    for destination_exists in [false, true] {
        for fault in PRE_PERSIST_FAULTS {
            let directory = tempfile::tempdir().unwrap();
            let output_path = directory.path().join("translated.pdf");
            if destination_exists {
                std::fs::write(&output_path, EXISTING).unwrap();
            }

            let failed = run_none_with_write_fault(&input, &output_path, fault);
            assert!(!failed.status.success(), "fault {fault} did not terminate");
            if destination_exists {
                assert_eq!(std::fs::read(&output_path).unwrap(), EXISTING, "{fault}");
            } else {
                assert!(!output_path.exists(), "{fault} published a destination");
            }

            let temporary_files = std::fs::read_dir(directory.path())
                .unwrap()
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(".mimus-") && name.ends_with(".pdf.tmp")
                })
                .count();
            if *fault == "oom_after_partial_write" {
                assert_eq!(failed.status.code(), Some(6));
                let events = parse_events(&failed.stdout);
                assert_one_terminal_last(&events, "error");
                assert_eq!(events.last().unwrap()["category"], "internal");
                assert_eq!(events.last().unwrap()["reason"], "output_build");
                assert_eq!(
                    temporary_files, 0,
                    "recoverable OOM must unwind and remove its temporary file"
                );
            } else {
                assert_eq!(
                    temporary_files, 1,
                    "abrupt death must leave exactly one unpublished temporary file"
                );
            }

            let retried = run_none(&input, Some(&output_path), true);
            assert!(
                retried.status.success(),
                "re-entry after {fault} failed: {}",
                String::from_utf8_lossy(&retried.stderr)
            );
            assert_eq!(std::fs::read(&output_path).unwrap(), expected, "{fault}");
        }
    }

    for destination_exists in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let output_path = directory.path().join("translated.pdf");
        if destination_exists {
            std::fs::write(&output_path, EXISTING).unwrap();
        }
        let killed = run_none_with_write_fault(&input, &output_path, "kill_after_persist");
        assert!(!killed.status.success());
        let published = std::fs::read(&output_path).unwrap();
        assert_eq!(published, expected, "post-persist death left partial bytes");

        let retried = run_none(&input, Some(&output_path), true);
        assert!(retried.status.success());
        assert_eq!(std::fs::read(&output_path).unwrap(), published);
    }
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

#[test]
fn bilingual_is_opt_in_interleaved_and_additive_in_v2() {
    let input = fixture_path("unit-write-08-bilingual-navigation");
    let directory = tempfile::tempdir().unwrap();
    let default_output = directory.path().join("default.pdf");
    let default = run_none(&input, Some(&default_output), true);
    assert!(
        default.status.success(),
        "{}",
        String::from_utf8_lossy(&default.stderr)
    );
    let default_events = parse_events(&default.stdout);
    assert_eq!(
        default_events
            .iter()
            .find(|event| event["event"] == "configuration_resolved")
            .unwrap()["bilingual"],
        false
    );
    assert_eq!(default_events.last().unwrap()["bilingual"], false);
    assert_eq!(default_events.last().unwrap()["pages"], 2);

    let bilingual_output = directory.path().join("bilingual.pdf");
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    configure_test_fonts(&mut command);
    command
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("OPENAI_API_KEY", "must-not-be-used")
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--layout",
            "single-line",
            "--bilingual",
            "--output",
        ])
        .arg(&bilingual_output)
        .arg(&input);
    let bilingual = command.output().unwrap();
    assert!(
        bilingual.status.success(),
        "{}",
        String::from_utf8_lossy(&bilingual.stderr)
    );
    assert!(bilingual.stderr.is_empty());
    let events = parse_events(&bilingual.stdout);
    assert_one_terminal_last(&events, "result");
    assert_eq!(
        events
            .iter()
            .find(|event| event["event"] == "configuration_resolved")
            .unwrap()["bilingual"],
        true
    );
    assert_eq!(events.last().unwrap()["bilingual"], true);
    assert_eq!(events.last().unwrap()["pages"], 4);

    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&bilingual_output).unwrap();
    assert!(output_bytes.starts_with(&input_bytes));
    let original = lopdf::Document::load(&input).unwrap();
    let output = lopdf::Document::load(&bilingual_output).unwrap();
    let pages = output.get_pages().into_values().collect::<Vec<_>>();
    assert_eq!(pages.len(), 4);
    assert_eq!(pages[0], (3, 0));
    assert_eq!(pages[2], (4, 0));
    assert!(pages[1].0 > original.max_id && pages[3].0 > original.max_id);
    for page_id in [(3, 0), (4, 0)] {
        assert_eq!(
            output.get_object(page_id).unwrap(),
            original.get_object(page_id).unwrap(),
            "source page {page_id:?} changed"
        );
    }
    for page_id in [pages[1], pages[3]] {
        assert!(!output.get_dictionary(page_id).unwrap().has(b"Annots"));
    }

    let outline_destination = output
        .get_dictionary((13, 0))
        .unwrap()
        .get(b"Dest")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(outline_destination[0], lopdf::Object::Reference(pages[3]));
    let goto_destination = output
        .get_dictionary((15, 0))
        .unwrap()
        .get(b"A")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"D")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(goto_destination[0], lopdf::Object::Reference(pages[1]));
    let link_destination = output
        .get_dictionary((17, 0))
        .unwrap()
        .get(b"A")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"D")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(link_destination[0], lopdf::Object::Reference(pages[3]));
    assert_eq!(
        output.get_object((18, 0)).unwrap(),
        original.get_object((18, 0)).unwrap(),
        "URI action changed"
    );
    let labels = output
        .get_dictionary((23, 0))
        .unwrap()
        .get(b"Nums")
        .unwrap()
        .as_array()
        .unwrap();
    let starts = labels
        .iter()
        .skip(1)
        .step_by(2)
        .map(|value| {
            value
                .as_dict()
                .unwrap()
                .get(b"St")
                .unwrap()
                .as_i64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(starts, vec![3, 3, 7, 7]);

    let qpdf = Command::new("qpdf")
        .arg("--check")
        .arg(&bilingual_output)
        .output()
        .unwrap();
    assert!(
        qpdf.status.success(),
        "{}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
}

#[test]
fn strip_link_borders_is_opt_in_typed_and_annotation_scoped() {
    let input = fixture_path("unit-write-07-link-borders");
    let input_bytes = std::fs::read(&input).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let default_output = directory.path().join("default.pdf");
    let default = run_none(&input, Some(&default_output), true);
    assert!(
        default.status.success(),
        "{}",
        String::from_utf8_lossy(&default.stderr)
    );
    assert!(
        std::fs::read(&default_output)
            .unwrap()
            .starts_with(&input_bytes)
    );
    for object in [10, 11, 12, 13, 14] {
        assert_eq!(
            qpdf_object(&default_output, &object.to_string()),
            qpdf_object(&input, &object.to_string()),
            "unflagged annotation object {object} changed"
        );
    }
    assert!(parse_events(&default.stdout).iter().all(|event| {
        event.get("id").and_then(serde_json::Value::as_str) != Some("link_borders_stripped")
    }));

    let stripped_output = directory.path().join("stripped.pdf");
    let mut command = Command::new(BIN);
    command.env(PDFIUM_ENV, pdfium_library());
    configure_test_fonts(&mut command);
    let stripped = command
        .args([
            "--json",
            "translate",
            "--backend",
            "none",
            "--layout",
            "single-line",
            "--strip-link-borders",
            "--output",
        ])
        .arg(&stripped_output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        stripped.status.success(),
        "{}",
        String::from_utf8_lossy(&stripped.stderr)
    );
    assert!(stripped.stderr.is_empty());
    let events = parse_events(&stripped.stdout);
    assert_one_terminal_last(&events, "result");
    let configuration = events
        .iter()
        .find(|event| event["event"] == "configuration_resolved")
        .unwrap();
    assert_eq!(configuration["strip_link_borders"], true);
    assert_eq!(events.last().unwrap()["strip_link_borders"], true);
    let diagnostic = events
        .iter()
        .find(|event| event["id"] == "link_borders_stripped")
        .unwrap();
    assert_eq!(diagnostic["annotation_count"], 2);

    let output_bytes = std::fs::read(&stripped_output).unwrap();
    assert!(output_bytes.starts_with(&input_bytes));
    for object in [12, 13, 14] {
        assert_eq!(
            qpdf_object(&stripped_output, &object.to_string()),
            qpdf_object(&input, &object.to_string()),
            "control annotation {object} changed"
        );
    }
    for object in [10, 11] {
        let dictionary =
            String::from_utf8(qpdf_object(&stripped_output, &object.to_string())).unwrap();
        assert!(dictionary.contains("/Border [ 0 0 0 ]"), "{dictionary}");
        assert!(!dictionary.contains("/BS"), "{dictionary}");
    }
    assert!(
        String::from_utf8(qpdf_object(&stripped_output, "10"))
            .unwrap()
            .contains("https://example.com/border")
    );
    assert!(
        String::from_utf8(qpdf_object(&stripped_output, "11"))
            .unwrap()
            .contains("/Dest [ 3 0 R /Fit ]")
    );
}

fn first_element_attributes(xml: &[u8], name: &[u8]) -> BTreeMap<String, String> {
    element_attributes(xml, name)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("element {} not found", String::from_utf8_lossy(name)))
}

fn element_attributes(xml: &[u8], name: &[u8]) -> Vec<BTreeMap<String, String>> {
    let mut reader = Reader::from_reader(xml);
    let mut result = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == name =>
            {
                result.push(
                    element
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
                        .collect(),
                );
            }
            Event::Eof => return result,
            _ => {}
        }
    }
}

fn mupdf_element_attributes(path: &Path, element: &[u8]) -> Vec<BTreeMap<String, String>> {
    let output = Command::new("mutool")
        .args(["draw", "-F", "stext", "-o", "-"])
        .arg(path)
        .arg("1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    element_attributes(&output.stdout, element)
}

fn mupdf_character_attributes(path: &Path) -> Vec<BTreeMap<String, String>> {
    mupdf_element_attributes(path, b"char")
}

fn mupdf_character_origins_for_text(path: &Path, text: &str) -> Vec<(f64, f64)> {
    let attributes = mupdf_character_attributes(path);
    let expected = text
        .chars()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    let start = attributes
        .windows(expected.len())
        .position(|window| {
            window
                .iter()
                .zip(&expected)
                .all(|(character, expected)| character.get("c") == Some(expected))
        })
        .unwrap_or_else(|| {
            panic!(
                "could not find {text:?} in MuPDF output for {}",
                path.display()
            )
        });
    attributes[start..start + expected.len()]
        .iter()
        .map(|character| (number(character, "x"), number(character, "y")))
        .collect()
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
