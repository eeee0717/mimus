use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{IoReason, MimusError, Result, RetryReason, TranslationReason, UsageReason};

pub(crate) mod cache;
pub(crate) mod executor;

pub const PARAGRAPH_PROMPT_VERSION: &str = "mimus-paragraph-v1";
pub const TERMS_PROMPT_VERSION: &str = "mimus-terms-v1";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Glossary {
    entries: BTreeMap<String, String>,
}

impl Glossary {
    pub fn from_toml(contents: &str) -> Result<Self> {
        let file = toml::from_str::<GlossaryFile>(contents).map_err(|_| {
            MimusError::usage(
                UsageReason::InvalidArguments,
                "user glossary is malformed TOML",
            )
        })?;
        if file.version != 1 {
            return Err(MimusError::usage(
                UsageReason::InvalidArguments,
                "user glossary version must be 1",
            ));
        }
        Self::from_records(file.terms, GlossarySource::User)
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path).map_err(|_| {
            MimusError::usage(
                UsageReason::InvalidArguments,
                format!("could not read glossary {}", path.display()),
            )
        })?;
        Self::from_toml(&contents)
    }

    pub fn canonical_toml(&self) -> String {
        toml::to_string_pretty(&GlossaryFile {
            version: 1,
            terms: self
                .entries
                .iter()
                .map(|(source, target)| GlossaryRecord {
                    source: source.clone(),
                    target: target.clone(),
                })
                .collect(),
        })
        .expect("the canonical glossary schema is always serializable")
    }

    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|_| {
            MimusError::io(
                IoReason::GlossaryWrite,
                format!("could not create glossary beside {}", path.display()),
            )
        })?;
        temporary
            .write_all(self.canonical_toml().as_bytes())
            .map_err(|_| {
                MimusError::io(
                    IoReason::GlossaryWrite,
                    format!("could not write glossary {}", path.display()),
                )
            })?;
        temporary.persist(path).map_err(|_| {
            MimusError::io(
                IoReason::GlossaryWrite,
                format!("could not publish glossary {}", path.display()),
            )
        })?;
        Ok(())
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.canonical_toml().as_bytes());
        let mut output = String::with_capacity(digest.len() * 2);
        for byte in digest {
            write!(output, "{byte:02x}").expect("writing into a String cannot fail");
        }
        output
    }

    #[must_use]
    pub fn merged(auto: Self, user: &Self) -> Self {
        let mut entries = auto.entries;
        entries.extend(user.entries.clone());
        Self { entries }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn entries(&self) -> &BTreeMap<String, String> {
        &self.entries
    }

    fn prompt_json(&self) -> String {
        serde_json::to_string(&self.entries)
            .expect("a glossary containing strings is always serializable")
    }

    fn from_backend_json(contents: &str) -> Result<Self> {
        let response = serde_json::from_str::<ExtractedGlossary>(contents).map_err(|_| {
            MimusError::translation(
                TranslationReason::MalformedResponse,
                "term extraction returned malformed JSON",
            )
        })?;
        Self::from_records(response.terms, GlossarySource::Backend)
    }

    fn from_records(records: Vec<GlossaryRecord>, source: GlossarySource) -> Result<Self> {
        let mut entries = BTreeMap::new();
        for record in records {
            if record.source.trim().is_empty() || record.target.trim().is_empty() {
                return Err(source.error("glossary entries must have non-empty source and target"));
            }
            if entries.insert(record.source, record.target).is_some() {
                return Err(source.error("glossary contains a duplicate source term"));
            }
        }
        Ok(Self { entries })
    }
}

#[derive(Clone, Copy)]
enum GlossarySource {
    User,
    Backend,
}

impl GlossarySource {
    fn error(self, message: &str) -> MimusError {
        match self {
            Self::User => MimusError::usage(UsageReason::InvalidArguments, message),
            Self::Backend => MimusError::translation(TranslationReason::MalformedResponse, message),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlossaryFile {
    version: u32,
    #[serde(default)]
    terms: Vec<GlossaryRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlossaryRecord {
    source: String,
    target: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractedGlossary {
    terms: Vec<GlossaryRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedPart {
    Text { text: String, bold: bool },
    Formula,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedTranslation {
    request_text: String,
    tokens: Vec<ProtocolToken>,
    echo_retry_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedTranslation(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranslationOutcome {
    Translated(ValidatedTranslation),
    Identity,
    PlaceholderViolation(PlaceholderViolation),
}

impl ValidatedTranslation {
    pub(crate) fn from_cache(value: String) -> Self {
        Self(value)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StyledCharacter {
    pub(crate) value: char,
    pub(crate) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoredTranslation {
    segments: Vec<Vec<StyledCharacter>>,
}

impl RestoredTranslation {
    pub(crate) fn plain_text(&self) -> String {
        self.segments
            .iter()
            .flatten()
            .map(|character| character.value)
            .collect()
    }

    pub(crate) fn segments(&self) -> &[Vec<StyledCharacter>] {
        &self.segments
    }
}
impl PreparedTranslation {
    pub(crate) fn new(parts: impl IntoIterator<Item = PreparedPart>) -> Self {
        let mut request_text = String::new();
        let mut tokens = Vec::new();
        let mut source_text = String::new();
        let mut formula_index = 0;
        let mut bold_index = 0;
        let mut literal_index = 0;
        for part in parts {
            match part {
                PreparedPart::Text { text, bold: false } => {
                    source_text.push_str(&text);
                    push_encoded_text(&mut request_text, &mut tokens, &mut literal_index, &text);
                }
                PreparedPart::Text { text, bold: true } => {
                    source_text.push_str(&text);
                    bold_index += 1;
                    let open = format!("<b{bold_index}>");
                    let close = format!("</b{bold_index}>");
                    request_text.push_str(&open);
                    push_encoded_text(&mut request_text, &mut tokens, &mut literal_index, &text);
                    request_text.push_str(&close);
                    tokens.push(ProtocolToken::BoldOpen {
                        index: bold_index,
                        literal: open,
                    });
                    tokens.push(ProtocolToken::BoldClose {
                        index: bold_index,
                        literal: close,
                    });
                }
                PreparedPart::Formula => {
                    formula_index += 1;
                    let literal = format!("{{v{formula_index}}}");
                    request_text.push_str(&literal);
                    tokens.push(ProtocolToken::Formula {
                        index: formula_index,
                        literal,
                    });
                }
            }
        }
        Self {
            request_text,
            tokens,
            echo_retry_eligible: echo_retry_eligible(&source_text),
        }
    }

    pub(crate) fn request_text(&self) -> &str {
        &self.request_text
    }

    pub(crate) const fn echo_retry_eligible(&self) -> bool {
        self.echo_retry_eligible
    }

    pub(crate) fn placeholder_retry_correction(
        &self,
        violation: PlaceholderViolation,
        output: &str,
    ) -> String {
        let expected = scan_tokens(&self.request_text).unwrap_or_default();
        let observed = scan_tokens(output).unwrap_or_default();
        let required = expected
            .iter()
            .map(|token| token.literal.clone())
            .collect::<Vec<_>>();
        let observed_counts = observed.iter().fold(BTreeMap::new(), |mut counts, token| {
            *counts.entry(token.literal.clone()).or_insert(0_usize) += 1;
            counts
        });
        match violation {
            PlaceholderViolation::Missing => {
                let missing = required
                    .iter()
                    .filter(|token| !observed_counts.contains_key(token.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                format!(
                    "the previous response omitted required placeholders: {}. include each missing placeholder exactly once.",
                    placeholder_list(&missing)
                )
            }
            PlaceholderViolation::Duplicate => {
                let duplicated = required
                    .iter()
                    .filter(|token| observed_counts.get(token.as_str()).copied().unwrap_or(0) > 1)
                    .cloned()
                    .collect::<Vec<_>>();
                format!(
                    "the previous response duplicated placeholders: {}. include every required placeholder exactly once.",
                    placeholder_list(&duplicated)
                )
            }
            PlaceholderViolation::Unknown => {
                let mut unknown = Vec::new();
                for token in observed
                    .iter()
                    .filter(|token| !required.contains(&token.literal))
                    .map(|token| token.literal.clone())
                {
                    if !unknown.contains(&token) {
                        unknown.push(token);
                    }
                    if unknown.len() == 8 {
                        break;
                    }
                }
                if required.is_empty() {
                    format!(
                        "the previous response introduced unknown placeholders: {}. do not emit placeholders because the input has none.",
                        placeholder_list(&unknown)
                    )
                } else {
                    format!(
                        "the previous response introduced unknown placeholders: {}. use only this required placeholder sequence: {}.",
                        placeholder_list(&unknown),
                        placeholder_list(&required)
                    )
                }
            }
            PlaceholderViolation::TagNesting => {
                let bold_tags = expected
                    .iter()
                    .filter(|token| {
                        matches!(
                            token.kind,
                            ScannedTokenKind::BoldOpen(_) | ScannedTokenKind::BoldClose(_)
                        )
                    })
                    .map(|token| token.literal.clone())
                    .collect::<Vec<_>>();
                format!(
                    "the previous response mis-nested bold placeholders. use this exact bold-tag order: {}.",
                    placeholder_list(&bold_tags)
                )
            }
            PlaceholderViolation::FormulaOrder => {
                let formulas = expected
                    .iter()
                    .filter(|token| matches!(token.kind, ScannedTokenKind::Formula(_)))
                    .map(|token| token.literal.clone())
                    .collect::<Vec<_>>();
                format!(
                    "the previous response changed formula placeholder order. use this exact formula order: {}. include each exactly once.",
                    placeholder_list(&formulas)
                )
            }
            PlaceholderViolation::PartialToken => format!(
                "the previous response contained a partial placeholder. emit only complete placeholders and use this required sequence: {}.",
                placeholder_list(&required)
            ),
            PlaceholderViolation::BackendEcho => {
                "the previous response echoed the input. return a translation while preserving every placeholder exactly."
                    .to_owned()
            }
        }
    }

    pub(crate) fn classify(&self, output: &str) -> TranslationOutcome {
        if output == self.request_text {
            return TranslationOutcome::Identity;
        }
        match self.validate(output, true) {
            Ok(validated) => TranslationOutcome::Translated(validated),
            Err(violation) => TranslationOutcome::PlaceholderViolation(violation),
        }
    }

    pub(crate) fn validate(
        &self,
        output: &str,
        allow_echo: bool,
    ) -> std::result::Result<ValidatedTranslation, PlaceholderViolation> {
        if !allow_echo && output == self.request_text {
            return Err(PlaceholderViolation::BackendEcho);
        }
        let observed = scan_tokens(output)?;
        let expected = self
            .tokens
            .iter()
            .map(ProtocolToken::literal)
            .collect::<std::collections::BTreeSet<_>>();
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        let mut formula_order = Vec::new();
        let mut bold_stack = Vec::new();
        for token in &observed {
            if !expected.contains(token.literal.as_str()) {
                return Err(PlaceholderViolation::Unknown);
            }
            *counts.entry(token.literal.as_str()).or_default() += 1;
            match token.kind {
                ScannedTokenKind::Formula(index) => formula_order.push(index),
                ScannedTokenKind::BoldOpen(index) => bold_stack.push(index),
                ScannedTokenKind::BoldClose(index) => {
                    if bold_stack.pop() != Some(index) {
                        return Err(PlaceholderViolation::TagNesting);
                    }
                }
                ScannedTokenKind::Literal => {}
            }
        }
        if !bold_stack.is_empty() {
            return Err(PlaceholderViolation::TagNesting);
        }
        for token in &self.tokens {
            match counts.get(token.literal()).copied().unwrap_or(0) {
                0 => return Err(PlaceholderViolation::Missing),
                1 => {}
                _ => return Err(PlaceholderViolation::Duplicate),
            }
        }
        let expected_formula_order = self
            .tokens
            .iter()
            .filter_map(|token| match token {
                ProtocolToken::Formula { index, .. } => Some(*index),
                _ => None,
            })
            .collect::<Vec<_>>();
        if formula_order != expected_formula_order {
            return Err(PlaceholderViolation::FormulaOrder);
        }

        Ok(ValidatedTranslation(output.to_owned()))
    }

    pub(crate) fn restore(
        &self,
        validated: &ValidatedTranslation,
    ) -> std::result::Result<RestoredTranslation, PlaceholderViolation> {
        let observed = scan_tokens(validated.as_str())?;
        let protocol = self
            .tokens
            .iter()
            .map(|token| (token.literal(), token))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut segments = vec![Vec::new()];
        let mut bold_stack = Vec::new();
        let mut cursor = 0;
        for token in observed {
            push_styled_text(
                segments.last_mut().unwrap(),
                &validated.as_str()[cursor..token.start],
                !bold_stack.is_empty(),
            );
            let Some(expected) = protocol.get(token.literal.as_str()) else {
                return Err(PlaceholderViolation::Unknown);
            };
            match expected {
                ProtocolToken::Formula { .. } => segments.push(Vec::new()),
                ProtocolToken::BoldOpen { index, .. } => bold_stack.push(*index),
                ProtocolToken::BoldClose { index, .. } => {
                    if bold_stack.pop() != Some(*index) {
                        return Err(PlaceholderViolation::TagNesting);
                    }
                }
                ProtocolToken::Literal { value, .. } => {
                    segments.last_mut().unwrap().push(StyledCharacter {
                        value: *value,
                        bold: !bold_stack.is_empty(),
                    })
                }
            }
            cursor = token.end;
        }
        push_styled_text(
            segments.last_mut().unwrap(),
            &validated.as_str()[cursor..],
            !bold_stack.is_empty(),
        );
        if !bold_stack.is_empty() {
            return Err(PlaceholderViolation::TagNesting);
        }
        Ok(RestoredTranslation { segments })
    }
}

fn echo_retry_eligible(source: &str) -> bool {
    let trimmed = source.trim();
    if trimmed.is_empty() || looks_like_email(trimmed) {
        return false;
    }
    trimmed.chars().any(char::is_alphabetic)
}

fn looks_like_email(source: &str) -> bool {
    if source.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = source.split('@');
    let Some(local) = parts.next() else {
        return false;
    };
    let Some(domain) = parts.next() else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && parts.next().is_none()
}

fn push_encoded_text(
    request_text: &mut String,
    tokens: &mut Vec<ProtocolToken>,
    literal_index: &mut usize,
    text: &str,
) {
    for value in text.chars() {
        if matches!(value, '{' | '<') {
            *literal_index += 1;
            let literal = format!("{{l{literal_index}}}");
            request_text.push_str(&literal);
            tokens.push(ProtocolToken::Literal { value, literal });
        } else {
            request_text.push(value);
        }
    }
}

fn push_styled_text(output: &mut Vec<StyledCharacter>, text: &str, bold: bool) {
    output.extend(text.chars().map(|value| StyledCharacter { value, bold }));
}

#[derive(Debug, Clone)]
enum ProtocolToken {
    Formula { index: usize, literal: String },
    BoldOpen { index: usize, literal: String },
    BoldClose { index: usize, literal: String },
    Literal { value: char, literal: String },
}

impl ProtocolToken {
    fn literal(&self) -> &str {
        match self {
            Self::Formula { literal, .. }
            | Self::BoldOpen { literal, .. }
            | Self::BoldClose { literal, .. }
            | Self::Literal { literal, .. } => literal,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlaceholderViolation {
    Missing,
    Duplicate,
    Unknown,
    TagNesting,
    FormulaOrder,
    PartialToken,
    BackendEcho,
}

impl PlaceholderViolation {
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Duplicate => "duplicate",
            Self::Unknown => "unknown",
            Self::TagNesting => "tag_nesting",
            Self::FormulaOrder => "formula_order",
            Self::PartialToken => "partial_token",
            Self::BackendEcho => "backend_echo",
        }
    }
}

struct ScannedToken {
    literal: String,
    kind: ScannedTokenKind,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RedactedTranslationProfile {
    pub response_bytes: usize,
    pub response_characters: usize,
    pub token_count: usize,
    pub token_scan_valid: bool,
}

pub(crate) fn redacted_translation_profile(output: &str) -> RedactedTranslationProfile {
    let tokens = scan_tokens(output);
    RedactedTranslationProfile {
        response_bytes: output.len(),
        response_characters: output.chars().count(),
        token_count: tokens.as_ref().map_or(0, Vec::len),
        token_scan_valid: tokens.is_ok(),
    }
}

#[derive(Clone, Copy)]
enum ScannedTokenKind {
    Formula(usize),
    BoldOpen(usize),
    BoldClose(usize),
    Literal,
}

fn scan_tokens(text: &str) -> std::result::Result<Vec<ScannedToken>, PlaceholderViolation> {
    let mut output = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let tail = &text[cursor..];
        let (prefix, terminator, kind) = if tail.starts_with("{v") {
            ("{v", '}', 0_u8)
        } else if tail.starts_with("{l") {
            ("{l", '}', 3_u8)
        } else if tail.starts_with("</b") {
            ("</b", '>', 2_u8)
        } else if tail.starts_with("<b") {
            ("<b", '>', 1_u8)
        } else {
            let character = tail
                .chars()
                .next()
                .expect("cursor is on a character boundary");
            cursor += character.len_utf8();
            continue;
        };
        let digits_start = cursor + prefix.len();
        let Some(relative_end) = text[digits_start..].find(terminator) else {
            return Err(PlaceholderViolation::PartialToken);
        };
        let digits_end = digits_start + relative_end;
        let digits = &text[digits_start..digits_end];
        let index = digits
            .parse::<usize>()
            .ok()
            .filter(|index| *index > 0)
            .ok_or(PlaceholderViolation::PartialToken)?;
        let end = digits_end + terminator.len_utf8();
        let token_kind = match kind {
            0 => ScannedTokenKind::Formula(index),
            1 => ScannedTokenKind::BoldOpen(index),
            2 => ScannedTokenKind::BoldClose(index),
            _ => ScannedTokenKind::Literal,
        };
        output.push(ScannedToken {
            literal: text[cursor..end].to_owned(),
            kind: token_kind,
            start: cursor,
            end,
        });
        cursor = end;
    }
    Ok(output)
}

fn placeholder_list(tokens: &[String]) -> String {
    if tokens.is_empty() {
        "none".to_owned()
    } else {
        tokens.join(", ")
    }
}

pub struct TranslationRequest<'a> {
    pub text: &'a str,
    pub target_language: &'a str,
    pub glossary: &'a Glossary,
    pub placeholder_correction: Option<&'a str>,
    pub content_correction: Option<&'a str>,
}

pub struct TermExtractionRequest<'a> {
    pub document_text: &'a str,
    pub target_language: &'a str,
}

pub trait Translator: Send + Sync {
    fn translate(&self, request: &TranslationRequest<'_>) -> Result<String>;

    fn model_id(&self) -> &str {
        "custom"
    }

    fn extract_terms(&self, _request: &TermExtractionRequest<'_>) -> Result<Glossary> {
        Ok(Glossary::default())
    }
}

pub trait Sleeper: Send + Sync + std::fmt::Debug {
    fn sleep(&self, duration: Duration);
}

#[derive(Debug, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
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
        let glossary = request.glossary.prompt_json();
        let mut instructions = format!(
            "Translate the input into {}. Return only the translated text. Preserve every placeholder exactly. Apply this source-to-target glossary JSON exactly: {glossary}",
            request.target_language
        );
        if let Some(correction) = request.placeholder_correction {
            write!(instructions, " Placeholder correction: {correction}")
                .expect("writing to a String cannot fail");
        }
        if let Some(correction) = request.content_correction {
            write!(
                instructions,
                " Content conservation correction: {correction}"
            )
            .expect("writing to a String cannot fail");
        }
        self.response_text(instructions, request.text)
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn extract_terms(&self, request: &TermExtractionRequest<'_>) -> Result<Glossary> {
        let output = self.response_text(
            format!(
                "Extract important technical terms and their translations into {}. Return only JSON with shape {{\"terms\":[{{\"source\":\"...\",\"target\":\"...\"}}]}}. Prompt version: {TERMS_PROMPT_VERSION}",
                request.target_language
            ),
            request.document_text,
        )?;
        Glossary::from_backend_json(&output)
    }
}

impl OpenAiTranslator {
    fn response_text(&self, instructions: String, input: &str) -> Result<String> {
        let payload = ResponsesRequest {
            model: &self.model,
            instructions,
            input,
        };
        let response = self
            .client
            .post(self.responses_url.clone())
            .bearer_auth(self.api_key.expose_secret())
            .json(&payload)
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    MimusError::retryable_translation(
                        TranslationReason::TransportFailure,
                        RetryReason::Timeout,
                        "OpenAI Responses API request timed out",
                    )
                } else {
                    MimusError::translation(
                        TranslationReason::TransportFailure,
                        "OpenAI Responses API request failed",
                    )
                }
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
    if status == StatusCode::TOO_MANY_REQUESTS {
        MimusError::retryable_translation(
            TranslationReason::BackendRejected,
            RetryReason::RateLimited,
            "OpenAI Responses API rate limit was reached",
        )
    } else if status == StatusCode::REQUEST_TIMEOUT {
        MimusError::retryable_translation(
            TranslationReason::BackendRejected,
            RetryReason::Timeout,
            "OpenAI Responses API request timed out",
        )
    } else if matches!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    ) {
        MimusError::retryable_translation(
            TranslationReason::TranslationFailed,
            RetryReason::ServerError,
            format!("OpenAI Responses API temporarily failed ({status})"),
        )
    } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
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
    use std::sync::{Arc, LazyLock, Mutex};
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
            Self::delayed(response, Duration::ZERO)
        }

        fn delayed(response: &'static [u8], delay: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let request = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&request);
            let thread = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                *captured.lock().unwrap() = read_request(&mut stream);
                thread::sleep(delay);
                let _ = stream.write_all(response);
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

    static EMPTY_GLOSSARY: LazyLock<Glossary> = LazyLock::new(Glossary::default);

    fn request() -> TranslationRequest<'static> {
        TranslationRequest {
            text: "Hello",
            target_language: "zh-CN",
            glossary: &EMPTY_GLOSSARY,
            placeholder_correction: None,
            content_correction: None,
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

    #[test]
    fn responses_timeout_is_marked_retryable_without_leaking_the_key() {
        let server = FakeServer::delayed(
            b"HTTP/1.1 200 OK\r\nContent-Length: 28\r\nConnection: close\r\n\r\n{\"output_text\":\"Translated\"}",
            Duration::from_millis(100),
        );
        let translator = OpenAiTranslator::new(
            &server.url,
            "test-model".to_owned(),
            SecretString::from(CANARY.to_owned()),
            Duration::from_millis(20),
        )
        .unwrap();

        let error = translator.translate(&request()).unwrap_err();

        assert_eq!(error.retry_reason(), Some(RetryReason::Timeout));
        assert!(!format!("{error:?}\n{error}").contains(CANARY));
    }

    #[test]
    fn only_declared_http_statuses_are_retryable() {
        for (status, reason) in [
            (StatusCode::TOO_MANY_REQUESTS, RetryReason::RateLimited),
            (StatusCode::REQUEST_TIMEOUT, RetryReason::Timeout),
            (StatusCode::INTERNAL_SERVER_ERROR, RetryReason::ServerError),
            (StatusCode::BAD_GATEWAY, RetryReason::ServerError),
            (StatusCode::SERVICE_UNAVAILABLE, RetryReason::ServerError),
            (StatusCode::GATEWAY_TIMEOUT, RetryReason::ServerError),
        ] {
            assert_eq!(classify_status(status).retry_reason(), Some(reason));
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::NOT_IMPLEMENTED,
        ] {
            assert_eq!(classify_status(status).retry_reason(), None);
        }
    }

    fn placeholder_protocol() -> PreparedTranslation {
        PreparedTranslation::new([
            PreparedPart::Text {
                text: "Alpha ".to_owned(),
                bold: false,
            },
            PreparedPart::Formula,
            PreparedPart::Text {
                text: " bold ".to_owned(),
                bold: true,
            },
            PreparedPart::Formula,
        ])
    }

    #[test]
    fn placeholder_protocol_restores_validated_text_without_redrawing_formulas_or_tags() {
        let protocol = placeholder_protocol();
        assert_eq!(protocol.request_text(), "Alpha {v1}<b1> bold </b1>{v2}");
        let validated = protocol
            .validate("Translated {v1}<b1> strong </b1>{v2}", false)
            .unwrap();
        assert_eq!(validated.as_str(), "Translated {v1}<b1> strong </b1>{v2}");
        let restored = protocol.restore(&validated).unwrap();
        assert_eq!(restored.plain_text(), "Translated  strong ");
        assert_eq!(restored.segments().len(), 3);
        assert!(restored.segments()[0].iter().all(|value| !value.bold));
        assert!(restored.segments()[1].iter().all(|value| value.bold));
        assert!(restored.segments()[2].is_empty());
    }

    #[test]
    fn literal_marker_syntax_is_encoded_and_restored_without_collision() {
        let protocol = PreparedTranslation::new([
            PreparedPart::Text {
                text: "literal {v1} <b1> {v".to_owned(),
                bold: false,
            },
            PreparedPart::Formula,
        ]);
        assert_eq!(protocol.request_text(), "literal {l1}v1} {l2}b1> {l3}v{v1}");
        let validated = protocol
            .validate("translated {l1}v1} {l2}b1> {l3}v{v1}", false)
            .unwrap();
        let restored = protocol.restore(&validated).unwrap();
        assert_eq!(restored.plain_text(), "translated {v1} <b1> {v");
        assert_eq!(restored.segments().len(), 2);
    }

    #[test]
    fn placeholder_protocol_rejects_every_declared_failure_mode() {
        let protocol = placeholder_protocol();
        for (output, expected) in [
            (
                "Translated <b1> strong </b1>{v2}",
                PlaceholderViolation::Missing,
            ),
            (
                "Translated {v1}{v1}<b1> strong </b1>{v2}",
                PlaceholderViolation::Duplicate,
            ),
            (
                "Translated {v1}<b1> strong </b1>{v2}{v3}",
                PlaceholderViolation::Unknown,
            ),
            (
                "Translated {v1}</b1> strong <b1>{v2}",
                PlaceholderViolation::TagNesting,
            ),
            (
                "Translated {v2}<b1> strong </b1>{v1}",
                PlaceholderViolation::FormulaOrder,
            ),
            (
                "Translated {v1}<b1> strong </b1>{v2",
                PlaceholderViolation::PartialToken,
            ),
            (
                "Alpha {v1}<b1> bold </b1>{v2}",
                PlaceholderViolation::BackendEcho,
            ),
        ] {
            assert_eq!(protocol.validate(output, false), Err(expected), "{output}");
        }
    }

    #[test]
    fn backend_echo_is_an_identity_outcome_instead_of_a_placeholder_violation() {
        let prepared = PreparedTranslation::new([PreparedPart::Text {
            text: "user@example.com".to_owned(),
            bold: false,
        }]);

        assert_eq!(
            prepared.classify("user@example.com"),
            TranslationOutcome::Identity
        );
        assert!(matches!(
            prepared.classify("translated"),
            TranslationOutcome::Translated(_)
        ));
    }

    #[test]
    fn glossary_round_trip_is_canonical_and_user_entries_override_automatic_terms() {
        let automatic = Glossary::from_toml(
            "version = 1\n[[terms]]\nsource = 'zeta'\ntarget = 'auto-z'\n[[terms]]\nsource = 'alpha'\ntarget = 'auto-a'\n",
        )
        .unwrap();
        let user = Glossary::from_toml(
            "version = 1\n[[terms]]\nsource = 'zeta'\ntarget = 'user-z'\n[[terms]]\nsource = 'beta'\ntarget = 'user-b'\n",
        )
        .unwrap();
        let merged = Glossary::merged(automatic, &user);
        assert_eq!(merged.entries()["zeta"], "user-z");
        assert_eq!(merged.entries()["alpha"], "auto-a");
        assert_eq!(merged.entries()["beta"], "user-b");

        let canonical = merged.canonical_toml();
        assert!(canonical.find("alpha").unwrap() < canonical.find("beta").unwrap());
        assert!(canonical.find("beta").unwrap() < canonical.find("zeta").unwrap());
        let round_trip = Glossary::from_toml(&canonical).unwrap();
        assert_eq!(round_trip, merged);
        assert_eq!(round_trip.fingerprint(), merged.fingerprint());
        assert_eq!(merged.fingerprint().len(), 64);
    }

    #[test]
    fn malformed_user_glossaries_are_usage_errors() {
        for contents in [
            "version = 2",
            "version = 1\nunknown = true",
            "version = 1\n[[terms]]\nsource = ''\ntarget = 'x'",
            "version = 1\n[[terms]]\nsource = 'x'\ntarget = 'a'\n[[terms]]\nsource = 'x'\ntarget = 'b'",
        ] {
            let error = Glossary::from_toml(contents).unwrap_err();
            assert_eq!(error.category(), crate::error::ExitCategory::Usage);
        }
    }

    #[test]
    fn responses_term_extraction_uses_one_structured_request() {
        let server = FakeServer::one(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"output_text\":\"{\\\"terms\\\":[{\\\"source\\\":\\\"attention\\\",\\\"target\\\":\\\"attention-cn\\\"}]}\"}",
        );
        let glossary = translator(&server.url)
            .extract_terms(&TermExtractionRequest {
                document_text: "Attention is all you need.",
                target_language: "zh-CN",
            })
            .unwrap();
        assert_eq!(glossary.entries()["attention"], "attention-cn");
        let request = String::from_utf8(server.request.lock().unwrap().clone()).unwrap();
        assert_eq!(request.matches("POST /v1/responses").count(), 1);
        assert!(request.contains(TERMS_PROMPT_VERSION));
    }
}
