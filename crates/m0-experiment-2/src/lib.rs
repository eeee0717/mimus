mod tokenizer;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use tokenizer::{Token, TokenValue, tokenize};

const MAX_DECODED_STREAM_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct FixtureReport {
    pub fixture_id: String,
    pub streams: Vec<StreamTrace>,
    pub operators: Vec<OperatorTrace>,
    pub glyphs: Vec<GlyphTrace>,
    pub warnings: Vec<WalkError>,
    pub errors: Vec<WalkError>,
    pub manifest: ManifestComparison,
    pub pdfium: Option<PdfiumTrace>,
}

#[derive(Debug, Serialize)]
pub struct StreamTrace {
    pub object: u32,
    pub raw_hex: String,
    pub decoded_hex: String,
}

#[derive(Debug, Serialize)]
pub struct OperatorTrace {
    pub operator: String,
    pub raw_hex: String,
    pub operands: Vec<String>,
    pub ctm_before: Matrix,
    pub ctm_after: Matrix,
    pub text_matrix: Matrix,
    pub inline_image_payload_bytes: Option<usize>,
    pub inline_image_length_source: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GlyphTrace {
    pub unicode: Option<char>,
    pub cid: u16,
    pub gid: Option<u16>,
    pub baseline: [f64; 2],
    pub text_matrix: Matrix,
    pub ctm: Matrix,
    pub type3_metrics: Option<Type3MetricsTrace>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Type3MetricsTrace {
    pub width: [f64; 2],
    pub bbox: Option<[f64; 4]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalkError {
    pub id: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct ManifestComparison {
    pub expected_text: String,
    pub observed_text: String,
    pub text_matches: bool,
    pub expected_baseline: Option<[f64; 2]>,
    pub baseline_delta: Option<[f64; 2]>,
    pub tolerance_pt: f64,
    pub expected_cids: Vec<u16>,
    pub observed_cids: Vec<u16>,
    pub cid_sequence_matches: bool,
    pub declared_diagnostic: Option<String>,
    pub expected_diagnostics: Vec<String>,
    pub observed_diagnostics: Vec<String>,
    pub diagnostic_matches: bool,
}

#[derive(Debug, Serialize)]
pub struct PdfiumTrace {
    pub characters: Vec<PdfiumCharacter>,
    pub observed_text: String,
    pub text_matches_walk: bool,
    pub origin_deltas: Vec<[f64; 2]>,
}

#[derive(Debug, Serialize)]
pub struct PdfiumCharacter {
    pub unicode: Option<char>,
    pub origin: [f64; 2],
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Matrix(pub [f64; 6]);

impl Matrix {
    const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    fn from_operands(values: &[f64]) -> Self {
        Self([
            values[0], values[1], values[2], values[3], values[4], values[5],
        ])
    }

    /// Returns a transform that applies `inner`, then `self`.
    fn compose(self, inner: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [g, h, i, j, k, l] = inner.0;
        Self([
            a * g + c * h,
            b * g + d * h,
            a * i + c * j,
            b * i + d * j,
            a * k + c * l + e,
            b * k + d * l + f,
        ])
    }

    fn transform(self, point: [f64; 2]) -> [f64; 2] {
        let [a, b, c, d, e, f] = self.0;
        [
            a * point[0] + c * point[1] + e,
            b * point[0] + d * point[1] + f,
        ]
    }

    fn translated(self, x: f64, y: f64) -> Self {
        self.compose(Self([1.0, 0.0, 0.0, 1.0, x, y]))
    }
}

#[derive(Debug, Deserialize)]
struct Manifest {
    identity: Identity,
    source: Source,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Identity {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Source {
    pdf: String,
}

#[derive(Debug, Deserialize)]
struct Expected {
    tolerance_pt: f64,
    #[serde(default)]
    block: Vec<ExpectedBlock>,
    #[serde(default)]
    cid_sequence: Vec<u16>,
    declared_failure: Option<String>,
    #[serde(default)]
    operator_walk_diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ExpectedBlock {
    text: String,
    baseline_origin: Option<[f64; 2]>,
}

#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: Matrix,
    text_matrix: Matrix,
    line_matrix: Matrix,
    font_name: Vec<u8>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    rise: f64,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            text_matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            font_name: Vec::new(),
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            rise: 0.0,
        }
    }
}

struct Walker<'a> {
    document: &'a Document,
    resources: Dictionary,
    state: GraphicsState,
    stack: Vec<GraphicsState>,
    operands: Vec<Token>,
    operators: Vec<OperatorTrace>,
    glyphs: Vec<GlyphTrace>,
    warnings: Vec<WalkError>,
    errors: Vec<WalkError>,
    active_forms: Vec<ObjectId>,
    compatibility_depth: usize,
    in_type3_charproc: bool,
    type3_metrics: Option<Type3MetricsTrace>,
}

pub fn run_fixture(
    repo_root: &Path,
    fixture_id: &str,
    pdfium_library: Option<&Path>,
) -> Result<FixtureReport> {
    let fixture_dir = repo_root.join("corpus/fixtures").join(fixture_id);
    let manifest: Manifest = toml::from_str(
        &std::fs::read_to_string(fixture_dir.join("manifest.toml"))
            .with_context(|| format!("read manifest for {fixture_id}"))?,
    )
    .with_context(|| format!("parse manifest for {fixture_id}"))?;
    if manifest.identity.id != fixture_id {
        bail!("manifest ID does not match fixture directory");
    }

    let pdf_path = fixture_dir.join(&manifest.source.pdf);
    let document =
        Document::load(&pdf_path).with_context(|| format!("lopdf load {}", pdf_path.display()))?;
    let (_, page_id) = document
        .get_pages()
        .into_iter()
        .next()
        .context("fixture has no page")?;
    let page_tree_cycle = find_page_tree_cycle(&document, page_id)?;
    let resources = if page_tree_cycle.is_some() {
        Dictionary::new()
    } else {
        page_resources(&document, page_id)?
    };
    let mut walker = Walker {
        document: &document,
        resources,
        state: GraphicsState::default(),
        stack: Vec::new(),
        operands: Vec::new(),
        operators: Vec::new(),
        glyphs: Vec::new(),
        warnings: Vec::new(),
        errors: page_tree_cycle
            .as_ref()
            .map(|path| WalkError {
                id: "page-tree-cycle".into(),
                detail: format!("page tree object path {path:?}"),
            })
            .into_iter()
            .collect(),
        active_forms: Vec::new(),
        compatibility_depth: 0,
        in_type3_charproc: false,
        type3_metrics: None,
    };
    let mut streams = Vec::new();

    if page_tree_cycle.is_none() {
        for object_id in document.get_page_contents(page_id) {
            let stream = document
                .get_object(object_id)
                .and_then(Object::as_stream)
                .with_context(|| format!("page content object {} is not a stream", object_id.0))?;
            let decoded = stream
                .decompressed_content_with_limit(MAX_DECODED_STREAM_BYTES)
                .with_context(|| format!("decode content stream {}", object_id.0))?;
            streams.push(StreamTrace {
                object: object_id.0,
                raw_hex: hex(&stream.content),
                decoded_hex: hex(&decoded),
            });
            match tokenize(&decoded, object_id.0) {
                Ok(tokens) => walker.walk(tokens),
                Err(error) => {
                    walker.errors.push(error);
                    break;
                }
            }
        }
    }

    let expected_text = manifest
        .expected
        .block
        .iter()
        .map(|block| block.text.as_str())
        .collect::<String>();
    let observed_text = walker
        .glyphs
        .iter()
        .filter_map(|glyph| glyph.unicode)
        .collect::<String>();
    let expected_baseline = manifest
        .expected
        .block
        .first()
        .and_then(|block| block.baseline_origin);
    let baseline_delta = expected_baseline
        .zip(walker.glyphs.first().map(|glyph| glyph.baseline))
        .map(|(expected, actual)| [actual[0] - expected[0], actual[1] - expected[1]]);

    let mut pdfium = pdfium_library
        .map(|library| extract_pdfium_trace(&pdf_path, library))
        .transpose()?;
    if let Some(pdfium) = &mut pdfium {
        pdfium.observed_text = pdfium
            .characters
            .iter()
            .filter_map(|character| character.unicode)
            .filter(|character| !character.is_control())
            .collect();
        pdfium.text_matches_walk = pdfium.observed_text == observed_text;
        pdfium.origin_deltas = pdfium
            .characters
            .iter()
            .filter(|character| character.unicode.is_some_and(|value| !value.is_control()))
            .zip(&walker.glyphs)
            .map(|(pdfium, walked)| {
                [
                    pdfium.origin[0] - walked.baseline[0],
                    pdfium.origin[1] - walked.baseline[1],
                ]
            })
            .collect();
    }
    let observed_cids = walker
        .glyphs
        .iter()
        .map(|glyph| glyph.cid)
        .collect::<Vec<_>>();
    let declared_diagnostic = manifest
        .expected
        .declared_failure
        .as_deref()
        .and_then(|failure| failure.strip_prefix("operator-walk:"))
        .map(str::to_owned);
    let expected_diagnostics = if manifest.expected.operator_walk_diagnostics.is_empty() {
        declared_diagnostic.iter().cloned().collect::<BTreeSet<_>>()
    } else {
        manifest
            .expected
            .operator_walk_diagnostics
            .iter()
            .cloned()
            .collect()
    };
    let observed_diagnostics = walker
        .warnings
        .iter()
        .chain(&walker.errors)
        .map(|diagnostic| diagnostic.id.clone())
        .collect::<BTreeSet<_>>();
    let diagnostic_matches = expected_diagnostics == observed_diagnostics;

    Ok(FixtureReport {
        fixture_id: fixture_id.to_string(),
        streams,
        operators: walker.operators,
        glyphs: walker.glyphs,
        warnings: walker.warnings,
        errors: walker.errors,
        manifest: ManifestComparison {
            text_matches: expected_text == observed_text,
            expected_text,
            observed_text,
            expected_baseline,
            baseline_delta,
            tolerance_pt: manifest.expected.tolerance_pt,
            cid_sequence_matches: manifest.expected.cid_sequence.is_empty()
                || manifest.expected.cid_sequence == observed_cids,
            expected_cids: manifest.expected.cid_sequence,
            observed_cids,
            declared_diagnostic,
            expected_diagnostics: expected_diagnostics.into_iter().collect(),
            observed_diagnostics: observed_diagnostics.into_iter().collect(),
            diagnostic_matches,
        },
        pdfium,
    })
}

impl Walker<'_> {
    fn walk(&mut self, tokens: Vec<Token>) {
        for token in tokens {
            if let TokenValue::InlineImage {
                payload_bytes,
                length_source,
            } = token.value
            {
                let operands = std::mem::take(&mut self.operands);
                let matrix = self.state.ctm;
                if length_source.as_str() == "ei-scan" {
                    self.warn(
                        "inline-image-ei-scan",
                        "payload length required bounded EI terminator scanning",
                    );
                }
                self.operators.push(OperatorTrace {
                    operator: "BI..EI".into(),
                    raw_hex: hex(&token.raw),
                    operands: operands.iter().map(Token::display).collect(),
                    ctm_before: matrix,
                    ctm_after: matrix,
                    text_matrix: self.state.text_matrix,
                    inline_image_payload_bytes: Some(payload_bytes),
                    inline_image_length_source: Some(length_source.as_str().into()),
                });
            } else if let TokenValue::Keyword(operator) = &token.value {
                if let Some((first, second)) = split_double_decimal(operator) {
                    self.operands.push(Token {
                        value: TokenValue::Number(first),
                        raw: first.to_string().into_bytes(),
                    });
                    self.operands.push(Token {
                        value: TokenValue::Number(second),
                        raw: second.to_string().into_bytes(),
                    });
                    self.warn(
                        "double-decimal",
                        &format!(
                            "split {} as {first} and {second}",
                            String::from_utf8_lossy(operator)
                        ),
                    );
                    continue;
                }
                if let Some((number, recovered_operator)) = split_glued_operator(operator) {
                    self.operands.push(Token {
                        value: TokenValue::Number(number),
                        raw: number.to_string().into_bytes(),
                    });
                    let before = self.state.ctm;
                    let operands = std::mem::take(&mut self.operands);
                    self.apply_operator(recovered_operator, &operands);
                    self.warn(
                        "glued-token-recovery",
                        &format!(
                            "split {} as {number} and {}",
                            String::from_utf8_lossy(operator),
                            String::from_utf8_lossy(recovered_operator)
                        ),
                    );
                    self.operators.push(OperatorTrace {
                        operator: String::from_utf8_lossy(recovered_operator).into_owned(),
                        raw_hex: hex(&token.raw),
                        operands: operands.iter().map(Token::display).collect(),
                        ctm_before: before,
                        ctm_after: self.state.ctm,
                        text_matrix: self.state.text_matrix,
                        inline_image_payload_bytes: None,
                        inline_image_length_source: None,
                    });
                    continue;
                }
                let before = self.state.ctm;
                let operands = std::mem::take(&mut self.operands);
                self.apply_operator(operator, &operands);
                self.operators.push(OperatorTrace {
                    operator: String::from_utf8_lossy(operator).into_owned(),
                    raw_hex: hex(&token.raw),
                    operands: operands.iter().map(Token::display).collect(),
                    ctm_before: before,
                    ctm_after: self.state.ctm,
                    text_matrix: self.state.text_matrix,
                    inline_image_payload_bytes: None,
                    inline_image_length_source: None,
                });
            } else {
                self.operands.push(token);
            }
        }
    }

    fn apply_operator(&mut self, operator: &[u8], operands: &[Token]) {
        match operator {
            b"BX" => self.compatibility_depth += 1,
            b"EX" => {
                if self.compatibility_depth == 0 {
                    self.warn("compatibility-underflow", "EX has no matching BX");
                } else {
                    self.compatibility_depth -= 1;
                }
            }
            b"q" => self.stack.push(self.state.clone()),
            b"Q" => {
                if let Some(state) = self.stack.pop() {
                    self.state = state;
                } else {
                    self.warn("graphics-stack-underflow", "Q has no matching q");
                }
            }
            b"cm" => {
                if let Some(values) = self.numeric_tail(operator, operands, 6) {
                    self.state.ctm = self.state.ctm.compose(Matrix::from_operands(&values));
                }
            }
            b"BT" => {
                self.state.text_matrix = Matrix::IDENTITY;
                self.state.line_matrix = Matrix::IDENTITY;
            }
            b"Tf" => {
                if operands.len() < 2 {
                    self.warn("arity-short", "Tf requires a name and size");
                } else if let (TokenValue::Name(name), Some(size)) = (
                    &operands[operands.len() - 2].value,
                    operands.last().and_then(Token::number),
                ) {
                    self.state.font_name.clone_from(name);
                    self.state.font_size = size;
                    self.warn_excess(operator, operands.len(), 2);
                }
            }
            b"Tm" => {
                if let Some(values) = self.numeric_tail(operator, operands, 6) {
                    self.state.text_matrix = Matrix::from_operands(&values);
                    self.state.line_matrix = self.state.text_matrix;
                }
            }
            b"Td" => {
                if let Some(values) = self.numeric_tail(operator, operands, 2) {
                    self.state.line_matrix =
                        self.state.line_matrix.translated(values[0], values[1]);
                    self.state.text_matrix = self.state.line_matrix;
                }
            }
            b"Tc" => {
                if let Some(values) = self.numeric_tail(operator, operands, 1) {
                    self.state.char_spacing = values[0];
                }
            }
            b"Tw" => {
                if let Some(values) = self.numeric_tail(operator, operands, 1) {
                    self.state.word_spacing = values[0];
                }
            }
            b"Tz" => {
                if let Some(values) = self.numeric_tail(operator, operands, 1) {
                    self.state.horizontal_scale = values[0] / 100.0;
                }
            }
            b"Ts" => {
                if let Some(values) = self.numeric_tail(operator, operands, 1) {
                    self.state.rise = values[0];
                }
            }
            b"Tj" => {
                if let Some(Token {
                    value: TokenValue::String(bytes),
                    ..
                }) = operands.last()
                {
                    if operands.len() > 1 {
                        self.warn_excess(operator, operands.len(), 1);
                    }
                    self.show_text(bytes);
                } else {
                    self.warn("arity-short", "Tj requires one string");
                }
            }
            b"Do" => {
                if let Some(Token {
                    value: TokenValue::Name(name),
                    ..
                }) = operands.last()
                {
                    self.execute_form(name);
                } else {
                    self.warn("arity-short", "Do requires one name");
                }
            }
            b"d0" | b"d1" if !self.in_type3_charproc => self.warn(
                "type3-metrics-context",
                "d0/d1 appears outside a Type3 CharProc",
            ),
            b"d0" => {
                if let Some(values) = self.numeric_tail(operator, operands, 2) {
                    self.type3_metrics = Some(Type3MetricsTrace {
                        width: [values[0], values[1]],
                        bbox: None,
                    });
                }
            }
            b"d1" => {
                if let Some(values) = self.numeric_tail(operator, operands, 6) {
                    self.type3_metrics = Some(Type3MetricsTrace {
                        width: [values[0], values[1]],
                        bbox: Some([values[2], values[3], values[4], values[5]]),
                    });
                }
            }
            b"ET" => {}
            _ if is_known_operator(operator) => {}
            _ if self.compatibility_depth > 0 => {}
            _ => self.warn(
                "unknown-operator",
                &format!(
                    "{} appears outside BX/EX",
                    String::from_utf8_lossy(operator)
                ),
            ),
        }
    }

    fn show_text(&mut self, bytes: &[u8]) {
        let mut type3_metrics = BTreeMap::new();
        for byte in bytes
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            if let Some(metrics) = self.walk_type3_charproc(byte) {
                type3_metrics.insert(u16::from(byte), metrics);
            }
        }
        let Ok(glyphs) =
            decode_glyphs(self.document, &self.resources, &self.state.font_name, bytes)
        else {
            self.errors.push(WalkError {
                id: "missing-font".into(),
                detail: String::from_utf8_lossy(&self.state.font_name).into_owned(),
            });
            return;
        };
        for glyph in glyphs {
            let metrics = type3_metrics.get(&glyph.cid).cloned();
            let text_point = self.state.text_matrix.transform([0.0, self.state.rise]);
            let baseline = self.state.ctm.transform(text_point);
            self.glyphs.push(GlyphTrace {
                unicode: glyph.unicode,
                cid: glyph.cid,
                gid: glyph.gid,
                baseline,
                text_matrix: self.state.text_matrix,
                ctm: self.state.ctm,
                type3_metrics: metrics.clone(),
            });
            let word_spacing = if glyph.cid == u16::from(b' ') {
                self.state.word_spacing
            } else {
                0.0
            };
            let width = metrics
                .as_ref()
                .map_or(glyph.width, |metrics| metrics.width[0]);
            let advance =
                (width * self.state.font_size / 1000.0 + self.state.char_spacing + word_spacing)
                    * self.state.horizontal_scale;
            self.state.text_matrix = self.state.text_matrix.translated(advance, 0.0);
        }
    }

    fn walk_type3_charproc(&mut self, code: u8) -> Option<Type3MetricsTrace> {
        let resolved = (|| -> Result<Option<(ObjectId, lopdf::Stream, Dictionary)>> {
            let fonts = self
                .resources
                .get_deref(b"Font", self.document)
                .and_then(Object::as_dict)?;
            let font = fonts
                .get_deref(&self.state.font_name, self.document)
                .and_then(Object::as_dict)?;
            if !font
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|name| name == b"Type3")
            {
                return Ok(None);
            }
            let encoding = font
                .get_deref(b"Encoding", self.document)
                .and_then(Object::as_dict)?;
            let differences = encoding.get(b"Differences").and_then(Object::as_array)?;
            let mut current = 0u16;
            let mut glyph_name = None;
            for item in differences {
                match item {
                    Object::Integer(value) => current = u16::try_from(*value).unwrap_or(u16::MAX),
                    Object::Name(name) => {
                        if current == u16::from(code) {
                            glyph_name = Some(name.as_slice());
                            break;
                        }
                        current = current.saturating_add(1);
                    }
                    _ => {}
                }
            }
            let Some(glyph_name) = glyph_name else {
                return Ok(None);
            };
            let char_procs = font
                .get_deref(b"CharProcs", self.document)
                .and_then(Object::as_dict)?;
            let id = char_procs.get(glyph_name).and_then(Object::as_reference)?;
            let stream = self.document.get_object(id)?.as_stream()?.clone();
            let resources = font
                .get_deref(b"Resources", self.document)
                .and_then(Object::as_dict)
                .cloned()
                .unwrap_or_else(|_| self.resources.clone());
            Ok(Some((id, stream, resources)))
        })();
        let Ok(Some((id, stream, resources))) = resolved else {
            return None;
        };
        let Ok(decoded) = stream.decompressed_content_with_limit(MAX_DECODED_STREAM_BYTES) else {
            return None;
        };
        let Ok(tokens) = tokenize(&decoded, id.0) else {
            return None;
        };
        self.scoped_execution(
            resources,
            |walker| {
                walker.in_type3_charproc = true;
                walker.type3_metrics = None;
            },
            |walker| {
                walker.walk(tokens);
                walker.type3_metrics.clone()
            },
        )
    }

    fn execute_form(&mut self, name: &[u8]) {
        let resolved = (|| -> Result<(ObjectId, lopdf::Stream)> {
            let xobjects = self
                .resources
                .get_deref(b"XObject", self.document)
                .and_then(Object::as_dict)
                .context("resource dictionary has no XObject dictionary")?;
            let id = xobjects
                .get(name)
                .and_then(Object::as_reference)
                .with_context(|| {
                    format!(
                        "XObject {} is not an indirect reference",
                        String::from_utf8_lossy(name)
                    )
                })?;
            let stream = self.document.get_object(id)?.as_stream()?.clone();
            Ok((id, stream))
        })();
        let Ok((id, stream)) = resolved else {
            self.errors.push(WalkError {
                id: "missing-xobject".into(),
                detail: String::from_utf8_lossy(name).into_owned(),
            });
            return;
        };
        if self.active_forms.contains(&id) {
            let self_cycle = self.active_forms.last() == Some(&id);
            let mut path = self
                .active_forms
                .iter()
                .map(|object_id| object_id.0)
                .collect::<Vec<_>>();
            path.push(id.0);
            self.errors.push(WalkError {
                id: if self_cycle {
                    "recursive-form-self".into()
                } else {
                    "recursive-form-mutual".into()
                },
                detail: format!("active object path {path:?}"),
            });
            return;
        }
        if self.active_forms.len() >= 64 {
            self.errors.push(WalkError {
                id: "form-depth-64".into(),
                detail: "Form recursion exceeds 64 levels".into(),
            });
            return;
        }
        let bbox_is_valid = stream
            .dict
            .get(b"BBox")
            .and_then(Object::as_array)
            .is_ok_and(|bbox| {
                bbox.len() == 4 && bbox.iter().all(|value| object_number(value).is_some())
            });
        if !bbox_is_valid {
            self.errors.push(WalkError {
                id: "form-missing-bbox".into(),
                detail: format!(
                    "XObject {} object {} has no valid BBox",
                    String::from_utf8_lossy(name),
                    id.0
                ),
            });
            return;
        }
        let matrix = stream
            .dict
            .get(b"Matrix")
            .and_then(Object::as_array)
            .ok()
            .and_then(|values| values.iter().map(object_number).collect::<Option<Vec<_>>>())
            .filter(|values| values.len() == 6)
            .map(|values| Matrix::from_operands(&values))
            .unwrap_or(Matrix::IDENTITY);
        let resources = stream
            .dict
            .get_deref(b"Resources", self.document)
            .and_then(Object::as_dict)
            .cloned()
            .unwrap_or_else(|_| self.resources.clone());
        let decoded = match stream.decompressed_content_with_limit(MAX_DECODED_STREAM_BYTES) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.errors.push(WalkError {
                    id: "form-decode".into(),
                    detail: error.to_string(),
                });
                return;
            }
        };
        let tokens = match tokenize(&decoded, id.0) {
            Ok(tokens) => tokens,
            Err(error) => {
                self.errors.push(error);
                return;
            }
        };

        self.active_forms.push(id);
        self.scoped_execution(
            resources,
            |walker| walker.state.ctm = walker.state.ctm.compose(matrix),
            |walker| walker.walk(tokens),
        );
        self.active_forms.pop();
    }

    fn scoped_execution<R>(
        &mut self,
        resources: Dictionary,
        configure: impl FnOnce(&mut Self),
        execute: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let saved_state = self.state.clone();
        let saved_resources = std::mem::replace(&mut self.resources, resources);
        let saved_stack = std::mem::take(&mut self.stack);
        let saved_operands = std::mem::take(&mut self.operands);
        let saved_compatibility_depth = std::mem::take(&mut self.compatibility_depth);
        let saved_in_type3_charproc = std::mem::take(&mut self.in_type3_charproc);
        let saved_type3_metrics = self.type3_metrics.take();

        configure(self);
        let result = execute(self);

        if !self.stack.is_empty() {
            self.warn(
                "scoped-graphics-stack-unbalanced",
                &format!("discarding {} child graphics states", self.stack.len()),
            );
        }
        if !self.operands.is_empty() {
            self.warn(
                "scoped-operands-discarded",
                &format!("discarding {} child operands", self.operands.len()),
            );
        }
        if self.compatibility_depth != 0 {
            self.warn(
                "scoped-compatibility-unbalanced",
                &format!("discarding child BX/EX depth {}", self.compatibility_depth),
            );
        }

        self.type3_metrics = saved_type3_metrics;
        self.in_type3_charproc = saved_in_type3_charproc;
        self.compatibility_depth = saved_compatibility_depth;
        self.operands = saved_operands;
        self.stack = saved_stack;
        self.resources = saved_resources;
        self.state = saved_state;
        result
    }

    fn numeric_tail(
        &mut self,
        operator: &[u8],
        operands: &[Token],
        arity: usize,
    ) -> Option<Vec<f64>> {
        if operands.len() < arity {
            self.warn(
                "arity-short",
                &format!(
                    "{} requires {arity} operands, got {}",
                    String::from_utf8_lossy(operator),
                    operands.len()
                ),
            );
            return None;
        }
        self.warn_excess(operator, operands.len(), arity);
        let values = operands[operands.len() - arity..]
            .iter()
            .map(Token::number)
            .collect::<Option<Vec<_>>>();
        if values.is_none() {
            self.warn("operand-type", "numeric operator received a non-number");
        }
        values
    }

    fn warn_excess(&mut self, operator: &[u8], actual: usize, expected: usize) {
        if actual > expected {
            self.warn(
                "arity-excess",
                &format!(
                    "{} uses tail {expected} of {actual} operands",
                    String::from_utf8_lossy(operator)
                ),
            );
        }
    }

    fn warn(&mut self, id: &str, detail: &str) {
        self.warnings.push(WalkError {
            id: id.into(),
            detail: detail.into(),
        });
    }
}

struct DecodedGlyph {
    cid: u16,
    gid: Option<u16>,
    unicode: Option<char>,
    width: f64,
}

fn decode_glyphs(
    document: &Document,
    resources: &Dictionary,
    name: &[u8],
    bytes: &[u8],
) -> Result<Vec<DecodedGlyph>> {
    let fonts = resources
        .get_deref(b"Font", document)
        .and_then(Object::as_dict)
        .context("resource dictionary has no Font dictionary")?;
    let dictionary = fonts
        .get_deref(name, document)
        .and_then(Object::as_dict)
        .with_context(|| format!("font {} not found", String::from_utf8_lossy(name)))?;
    let is_type0 = dictionary
        .get(b"Subtype")
        .and_then(Object::as_name)
        .is_ok_and(|name| name == b"Type0");
    if !is_type0 {
        let first = dictionary
            .get(b"FirstChar")
            .and_then(Object::as_i64)
            .unwrap_or(0);
        let widths = dictionary.get(b"Widths").and_then(Object::as_array).ok();
        let base_font = dictionary.get(b"BaseFont").and_then(Object::as_name).ok();
        return Ok(bytes
            .iter()
            .map(|byte| {
                let cid = u16::from(*byte);
                let index = i64::from(cid) - first;
                let width = widths
                    .and_then(|values| {
                        usize::try_from(index)
                            .ok()
                            .and_then(|index| values.get(index))
                    })
                    .and_then(object_number)
                    .or_else(|| standard14_width(base_font?, *byte))
                    .unwrap_or(0.0);
                DecodedGlyph {
                    cid,
                    gid: None,
                    unicode: char::from_u32(u32::from(*byte)),
                    width,
                }
            })
            .collect());
    }
    if bytes.len() & 1 == 1 {
        bail!("Identity-H string has an odd number of bytes");
    }
    let descendant = dictionary
        .get(b"DescendantFonts")
        .and_then(Object::as_array)?
        .first()
        .context("Type0 font has no descendant")?;
    let (_, descendant) = document.dereference(descendant)?;
    let descendant = descendant.as_dict()?;
    let descriptor = descendant
        .get_deref(b"FontDescriptor", document)?
        .as_dict()?;
    let font_stream = descriptor.get_deref(b"FontFile2", document)?.as_stream()?;
    let font_bytes = font_stream.decompressed_content_with_limit(MAX_DECODED_STREAM_BYTES)?;
    let face = ttf_parser::Face::parse(&font_bytes, 0).context("parse embedded TrueType font")?;
    let mut inverse = std::collections::BTreeMap::new();
    if let Some(cmap) = face.tables().cmap {
        for subtable in cmap
            .subtables
            .into_iter()
            .filter(|subtable| subtable.is_unicode())
        {
            subtable.codepoints(|codepoint| {
                if let Some(gid) = subtable.glyph_index(codepoint)
                    && let Some(character) = char::from_u32(codepoint)
                {
                    inverse.entry(gid.0).or_insert(character);
                }
            });
        }
    }
    Ok(bytes
        .chunks(2)
        .map(|pair| {
            let cid = u16::from_be_bytes([pair[0], pair[1]]);
            let gid = cid;
            DecodedGlyph {
                cid,
                gid: Some(gid),
                unicode: inverse.get(&gid).copied(),
                width: cid_width(descendant, cid),
            }
        })
        .collect())
}

fn standard14_width(base_font: &[u8], byte: u8) -> Option<f64> {
    match base_font {
        b"Courier" | b"Courier-Bold" | b"Courier-Oblique" | b"Courier-BoldOblique" => Some(600.0),
        b"Helvetica" => match byte {
            b'I' => Some(278.0),
            b'H' => Some(722.0),
            _ => None,
        },
        _ => None,
    }
}

fn cid_width(dictionary: &Dictionary, cid: u16) -> f64 {
    let Ok(values) = dictionary.get(b"W").and_then(Object::as_array) else {
        return 1000.0;
    };
    let mut index = 0;
    while index < values.len() {
        let Some(first) = values
            .get(index)
            .and_then(object_number)
            .map(|value| value as u16)
        else {
            break;
        };
        let Some(next) = values.get(index + 1) else {
            break;
        };
        if let Ok(widths) = next.as_array() {
            if cid >= first
                && let Some(width) = widths.get(usize::from(cid - first)).and_then(object_number)
            {
                return width;
            }
            index += 2;
        } else if let (Some(last), Some(width)) = (
            object_number(next).map(|value| value as u16),
            values.get(index + 2).and_then(object_number),
        ) {
            if (first..=last).contains(&cid) {
                return width;
            }
            index += 3;
        } else {
            break;
        }
    }
    dictionary
        .get(b"DW")
        .ok()
        .and_then(object_number)
        .unwrap_or(1000.0)
}

fn page_resources(document: &Document, page_id: ObjectId) -> Result<Dictionary> {
    let mut current = page_id;
    for _ in 0..128 {
        let page = document.get_dictionary(current)?;
        if let Ok(resources) = page
            .get_deref(b"Resources", document)
            .and_then(Object::as_dict)
        {
            return Ok(resources.clone());
        }
        current = page
            .get(b"Parent")
            .and_then(Object::as_reference)
            .context("page tree has no Resources")?;
    }
    bail!("page resource inheritance exceeds 128 levels")
}

fn find_page_tree_cycle(document: &Document, page_id: ObjectId) -> Result<Option<Vec<u32>>> {
    let mut current = page_id;
    let mut visited = BTreeSet::new();
    let mut path = Vec::new();
    for _ in 0..128 {
        if !visited.insert(current) {
            path.push(current.0);
            return Ok(Some(path));
        }
        path.push(current.0);
        let node = document.get_dictionary(current)?;
        let Ok(parent) = node.get(b"Parent").and_then(Object::as_reference) else {
            return Ok(None);
        };
        current = parent;
    }
    bail!("page tree inheritance exceeds 128 levels")
}

fn object_number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn is_known_operator(operator: &[u8]) -> bool {
    matches!(
        operator,
        b"w" | b"J"
            | b"j"
            | b"M"
            | b"d"
            | b"ri"
            | b"i"
            | b"gs"
            | b"m"
            | b"l"
            | b"c"
            | b"v"
            | b"y"
            | b"h"
            | b"re"
            | b"S"
            | b"s"
            | b"f"
            | b"F"
            | b"f*"
            | b"B"
            | b"B*"
            | b"b"
            | b"b*"
            | b"n"
            | b"W"
            | b"W*"
            | b"TL"
            | b"Tr"
            | b"T*"
            | b"TJ"
            | b"'"
            | b"\""
            | b"CS"
            | b"cs"
            | b"SC"
            | b"SCN"
            | b"sc"
            | b"scn"
            | b"G"
            | b"g"
            | b"RG"
            | b"rg"
            | b"K"
            | b"k"
            | b"sh"
            | b"MP"
            | b"DP"
            | b"BMC"
            | b"BDC"
            | b"EMC"
    )
}

fn split_glued_operator(token: &[u8]) -> Option<(f64, &'static [u8])> {
    const OPERATORS: [&[u8]; 8] = [b"Tm", b"Td", b"Tf", b"Tc", b"Tw", b"Tz", b"Ts", b"cm"];
    OPERATORS.iter().find_map(|operator| {
        let prefix = token.strip_suffix(*operator)?;
        (!prefix.is_empty()).then(|| {
            std::str::from_utf8(prefix)
                .ok()?
                .parse()
                .ok()
                .map(|number| (number, *operator))
        })?
    })
}

fn split_double_decimal(token: &[u8]) -> Option<(f64, f64)> {
    let text = std::str::from_utf8(token).ok()?;
    let mut dots = text.match_indices('.').map(|(index, _)| index);
    let _first = dots.next()?;
    let second = dots.next()?;
    if dots.next().is_some() {
        return None;
    }
    let first = text[..second].parse().ok()?;
    let second = text[second..].parse().ok()?;
    Some((first, second))
}

fn extract_pdfium_trace(pdf_path: &Path, library: &Path) -> Result<PdfiumTrace> {
    static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
    if PDFIUM.get().is_none() {
        let bindings = Pdfium::bind_to_library(library)
            .with_context(|| format!("bind PDFium library {}", library.display()))?;
        let _ = PDFIUM.set(Pdfium::new(bindings));
    }
    let pdfium = PDFIUM.get().context("initialize PDFium")?;
    let document = pdfium
        .load_pdf_from_file(pdf_path, None)
        .with_context(|| format!("PDFium load {}", pdf_path.display()))?;
    let page = document
        .pages()
        .first()
        .context("PDFium fixture has no page")?;
    let text = page.text().context("PDFium load text page")?;
    let mut characters = Vec::new();
    for character in text.chars().iter() {
        let (x, y) = character.origin().context("PDFium character origin")?;
        characters.push(PdfiumCharacter {
            unicode: character.unicode_char(),
            origin: [f64::from(x.value), f64::from(y.value)],
        });
    }
    Ok(PdfiumTrace {
        characters,
        observed_text: String::new(),
        text_matches_walk: false,
        origin_deltas: Vec::new(),
    })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}
