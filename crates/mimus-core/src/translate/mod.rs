use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::error::{MimusError, Result, TranslationReason, UsageReason};

pub const PARAGRAPH_PROMPT_VERSION: &str = "mimus-paragraph-v1";

pub struct TranslationRequest<'a> {
    pub text: &'a str,
    pub target_language: &'a str,
}

pub trait Translator: Send + Sync {
    fn translate(&self, request: &TranslationRequest<'_>) -> Result<String>;

    fn model_id(&self) -> &str {
        "custom"
    }
}

#[derive(Debug, Default)]
pub struct NoneTranslator;

impl Translator for NoneTranslator {
    fn translate(&self, request: &TranslationRequest<'_>) -> Result<String> {
        Ok(request.text.to_owned())
    }

    fn model_id(&self) -> &str {
        "none"
    }
}

pub struct OpenAiTranslator {
    client: Client,
    responses_url: reqwest::Url,
    model: String,
    api_key: SecretString,
}

impl OpenAiTranslator {
    pub fn new(
        base_url: &str,
        model: String,
        api_key: SecretString,
        timeout: Duration,
    ) -> Result<Self> {
        if model.trim().is_empty() {
            return Err(MimusError::usage(
                UsageReason::InvalidArguments,
                "OpenAI model must not be empty",
            ));
        }
        if api_key.expose_secret().trim().is_empty() {
            return Err(MimusError::usage(
                UsageReason::InvalidArguments,
                "OpenAI API key is required",
            )
            .with_hint("set API_KEY or configure api_key in ~/.config/mimus/config.toml"));
        }
        let responses_url = responses_url(base_url)?;
        let client = Client::builder().timeout(timeout).build().map_err(|_| {
            MimusError::translation(
                TranslationReason::TransportFailure,
                "could not initialize the OpenAI HTTP client",
            )
        })?;
        Ok(Self {
            client,
            responses_url,
            model,
            api_key,
        })
    }

    #[must_use]
    pub fn responses_url(&self) -> &reqwest::Url {
        &self.responses_url
    }
}

pub fn validate_openai_base_url(base_url: &str) -> Result<()> {
    responses_url(base_url).map(|_| ())
}

impl Translator for OpenAiTranslator {
    fn translate(&self, request: &TranslationRequest<'_>) -> Result<String> {
        let payload = ResponsesRequest {
            model: &self.model,
            instructions: format!(
                "Translate the input into {}. Return only the translated text. Preserve every placeholder exactly.",
                request.target_language
            ),
            input: request.text,
        };
        let response = self
            .client
            .post(self.responses_url.clone())
            .bearer_auth(self.api_key.expose_secret())
            .json(&payload)
            .send()
            .map_err(|_| {
                MimusError::translation(
                    TranslationReason::TransportFailure,
                    "OpenAI Responses API request failed",
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(classify_status(status));
        }
        let response = response.json::<ResponsesResponse>().map_err(|_| {
            MimusError::translation(
                TranslationReason::MalformedResponse,
                "OpenAI Responses API returned malformed JSON",
            )
        })?;
        response.output_text().ok_or_else(|| {
            MimusError::translation(
                TranslationReason::MalformedResponse,
                "OpenAI Responses API returned no output_text content",
            )
        })
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

fn responses_url(base_url: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url).map_err(|_| {
        MimusError::usage(
            UsageReason::InvalidArguments,
            "OpenAI base URL must be a valid absolute URL",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(MimusError::usage(
            UsageReason::InvalidArguments,
            "OpenAI base URL must use http or https and include a host",
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(MimusError::usage(
            UsageReason::InvalidArguments,
            "OpenAI base URL must not contain credentials, a query, or a fragment",
        ));
    }
    let path = url.path().trim_end_matches('/');
    let resolved = if path == "/v1/responses" {
        path.to_owned()
    } else if path.ends_with("/v1") {
        format!("{path}/responses")
    } else if path.is_empty() {
        "/v1/responses".to_owned()
    } else {
        format!("{path}/v1/responses")
    };
    url.set_path(&resolved);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn classify_status(status: StatusCode) -> MimusError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        MimusError::translation(
            TranslationReason::AuthenticationFailed,
            format!("OpenAI Responses API authentication failed ({status})"),
        )
    } else if status.is_client_error() {
        MimusError::translation(
            TranslationReason::BackendRejected,
            format!("OpenAI Responses API rejected the request ({status})"),
        )
    } else {
        MimusError::translation(
            TranslationReason::TranslationFailed,
            format!("OpenAI Responses API failed ({status})"),
        )
    }
}

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    instructions: String,
    input: &'a str,
}

#[derive(Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    output: Vec<ResponseOutput>,
}

impl ResponsesResponse {
    fn output_text(self) -> Option<String> {
        self.output_text
            .filter(|text| !text.is_empty())
            .or_else(|| {
                let text = self
                    .output
                    .into_iter()
                    .flat_map(|item| item.content)
                    .filter(|content| content.kind == "output_text")
                    .filter_map(|content| content.text)
                    .collect::<String>();
                (!text.is_empty()).then_some(text)
            })
    }
}

#[derive(Deserialize)]
struct ResponseOutput {
    #[serde(default)]
    content: Vec<ResponseContent>,
}

#[derive(Deserialize)]
struct ResponseContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    const CANARY: &str = "mimus-secret-canary-never-print";

    struct FakeServer {
        url: String,
        request: Arc<Mutex<Vec<u8>>>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl FakeServer {
        fn one(response: &'static [u8]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let request = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&request);
            let thread = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                *captured.lock().unwrap() = read_request(&mut stream);
                stream.write_all(response).unwrap();
            });
            Self {
                url,
                request,
                thread: Some(thread),
            }
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
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
                    line.strip_prefix("content-length: ")
                        .or_else(|| line.strip_prefix("Content-Length: "))
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        request
    }

    fn translator(url: &str) -> OpenAiTranslator {
        OpenAiTranslator::new(
            url,
            "test-model".to_owned(),
            SecretString::from(CANARY.to_owned()),
            Duration::from_secs(2),
        )
        .unwrap()
    }

    fn request<'a>() -> TranslationRequest<'a> {
        TranslationRequest {
            text: "Hello",
            target_language: "zh-CN",
        }
    }

    #[test]
    fn none_translator_is_an_offline_identity_adapter() {
        assert_eq!(NoneTranslator.translate(&request()).unwrap(), "Hello");
        assert_eq!(NoneTranslator.model_id(), "none");
    }

    #[test]
    fn base_urls_resolve_to_responses_not_chat_completions() {
        for (base, expected) in [
            ("https://example.test", "https://example.test/v1/responses"),
            (
                "https://example.test/v1",
                "https://example.test/v1/responses",
            ),
            (
                "https://example.test/custom",
                "https://example.test/custom/v1/responses",
            ),
            (
                "https://example.test/v1/responses",
                "https://example.test/v1/responses",
            ),
            (
                "https://example.test/custom/responses",
                "https://example.test/custom/responses/v1/responses",
            ),
        ] {
            assert_eq!(responses_url(base).unwrap().as_str(), expected);
        }
    }

    #[test]
    fn base_urls_reject_components_that_can_carry_secrets() {
        for base in [
            "https://user:password@example.test/v1",
            "https://example.test/v1?token=secret",
            "https://example.test/v1#secret",
        ] {
            let error = responses_url(base).unwrap_err();
            assert_eq!(
                error.reason().as_str(),
                UsageReason::InvalidArguments.as_str()
            );
            let rendered = format!("{error:?}\n{error}");
            assert!(!rendered.contains("secret"));
            assert!(!rendered.contains("password"));
        }
    }

    #[test]
    fn responses_backend_sends_the_expected_wire_request_and_reads_output_text() {
        let server = FakeServer::one(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"translated\"}]}]}",
        );
        assert_eq!(
            translator(&server.url).translate(&request()).unwrap(),
            "translated"
        );
        let request = String::from_utf8(server.request.lock().unwrap().clone()).unwrap();
        assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer ")
        );
        assert!(request.contains(CANARY));
        assert!(request.contains("\"model\":\"test-model\""));
        assert!(request.contains("\"input\":\"Hello\""));
        assert!(!request.contains("chat/completions"));
    }

    #[test]
    fn backend_failures_are_classified_without_leaking_the_key_or_body() {
        for (response, reason) in [
            (
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 32\r\nConnection: close\r\n\r\nmimus-secret-canary-never-print".as_slice(),
                TranslationReason::AuthenticationFailed,
            ),
            (
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 32\r\nConnection: close\r\n\r\nmimus-secret-canary-never-print".as_slice(),
                TranslationReason::BackendRejected,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".as_slice(),
                TranslationReason::MalformedResponse,
            ),
        ] {
            let server = FakeServer::one(response);
            let error = translator(&server.url).translate(&request()).unwrap_err();
            assert_eq!(error.reason().as_str(), reason.as_str());
            let rendered = format!("{error:?}\n{error}");
            assert!(!rendered.contains(CANARY));
        }
    }

    #[test]
    fn transport_failure_is_classified_and_redacted() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let error = translator(&url).translate(&request()).unwrap_err();
        assert_eq!(
            error.reason().as_str(),
            TranslationReason::TransportFailure.as_str()
        );
        assert!(!format!("{error:?}\n{error}").contains(CANARY));
    }
}
