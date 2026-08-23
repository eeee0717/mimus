mod tokenizer;

use std::collections::BTreeSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::error::{ErrorReason, MimusError, Result};
use crate::il::{FontRef, Point, Rect, TextTransform};
use tokenizer::{Token, TokenKind, tokenize};

const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct WalkedChar {
    pub unicode: Option<char>,
    pub encoded: Vec<u8>,
    pub font: FontRef,
    pub font_size: f64,
    pub baseline_origin: Point,
    pub metric_box: Rect,
    pub text_transform: TextTransform,
    pub content_object: ObjectId,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Matrix([f64; 6]);

impl Matrix {
    const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    fn from_values(values: &[f64]) -> Self {
        Self([
            values[0], values[1], values[2], values[3], values[4], values[5],
        ])
    }

    fn then(self, inner: Self) -> Self {
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

    fn translate(self, x: f64, y: f64) -> Self {
        self.then(Self([1.0, 0.0, 0.0, 1.0, x, y]))
    }

    fn point(self, x: f64, y: f64) -> Point {
        let [a, b, c, d, e, f] = self.0;
        Point {
            x: a * x + c * y + e,
            y: b * x + d * y + f,
        }
    }
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
    in_text: bool,
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
            in_text: false,
        }
    }
}

struct Walker<'a> {
    document: &'a Document,
    resources: Dictionary,
    state: GraphicsState,
    stack: Vec<GraphicsState>,
    operands: Vec<Token>,
    characters: Vec<WalkedChar>,
    content_object: ObjectId,
}

pub fn walk_page(document: &Document, page_id: ObjectId) -> Result<Vec<WalkedChar>> {
    let resources = inherited_page_resources(document, page_id)?;
    let mut walker = Walker {
        document,
        resources,
        state: GraphicsState::default(),
        stack: Vec::new(),
        operands: Vec::new(),
        characters: Vec::new(),
        content_object: (0, 0),
    };
    for object_id in document.get_page_contents(page_id) {
        let stream = document
            .get_object(object_id)
            .and_then(Object::as_stream)
            .map_err(|error| {
                walk_error(format!(
                    "page content {} is not a stream: {error}",
                    object_id.0
                ))
            })?;
        let decoded = stream
            .decompressed_content_with_limit(MAX_STREAM_BYTES)
            .map_err(|error| {
                walk_error(format!("could not decode content {}: {error}", object_id.0))
            })?;
        walker.content_object = object_id;
        walker.walk(tokenize(&decoded)?)?;
    }
    Ok(walker.characters)
}

impl Walker<'_> {
    fn walk(&mut self, tokens: Vec<Token>) -> Result<()> {
        for token in tokens {
            if !matches!(token.kind, TokenKind::Operator(_)) {
                self.operands.push(token);
                continue;
            }
            let TokenKind::Operator(operator) = &token.kind else {
                unreachable!();
            };
            let operands = std::mem::take(&mut self.operands);
            match operator.as_slice() {
                b"q" => self.stack.push(self.state.clone()),
                b"Q" => {
                    self.state = self
                        .stack
                        .pop()
                        .ok_or_else(|| walk_error("graphics state stack underflow"))?;
                }
                b"cm" => {
                    let values = numeric_tail(&operands, 6, "cm")?;
                    self.state.ctm = self.state.ctm.then(Matrix::from_values(&values));
                }
                b"BT" => {
                    self.state.text_matrix = Matrix::IDENTITY;
                    self.state.line_matrix = Matrix::IDENTITY;
                    self.state.in_text = true;
                }
                b"ET" => self.state.in_text = false,
                b"Tf" => {
                    let tail = operand_tail(&operands, 2, "Tf")?;
                    let TokenKind::Name(name) = &tail[0].kind else {
                        return Err(walk_error("Tf font operand is not a name"));
                    };
                    let TokenKind::Number(size) = tail[1].kind else {
                        return Err(walk_error("Tf size operand is not a number"));
                    };
                    self.state.font_name.clone_from(name);
                    self.state.font_size = size;
                }
                b"Tm" => {
                    let values = numeric_tail(&operands, 6, "Tm")?;
                    let matrix = Matrix::from_values(&values);
                    self.state.text_matrix = matrix;
                    self.state.line_matrix = matrix;
                }
                b"Td" => {
                    let values = numeric_tail(&operands, 2, "Td")?;
                    self.state.line_matrix = self.state.line_matrix.translate(values[0], values[1]);
                    self.state.text_matrix = self.state.line_matrix;
                }
                b"Tc" => self.state.char_spacing = numeric_tail(&operands, 1, "Tc")?[0],
                b"Tw" => self.state.word_spacing = numeric_tail(&operands, 1, "Tw")?[0],
                b"Tz" => self.state.horizontal_scale = numeric_tail(&operands, 1, "Tz")?[0] / 100.0,
                b"Ts" => self.state.rise = numeric_tail(&operands, 1, "Ts")?[0],
                b"Tj" => {
                    let tail = operand_tail(&operands, 1, "Tj")?;
                    let TokenKind::Bytes(bytes) = &tail[0].kind else {
                        return Err(walk_error("Tj operand is not a string"));
                    };
                    self.show_text(bytes, tail[0].span.start, tail[0].span.end)?;
                }
                _ => {}
            }
        }
        if !self.operands.is_empty() {
            return Err(walk_error("content stream ended with unused operands"));
        }
        Ok(())
    }

    fn show_text(&mut self, bytes: &[u8], byte_start: usize, byte_end: usize) -> Result<()> {
        if !self.state.in_text {
            return Err(walk_error("Tj appeared outside BT/ET"));
        }
        if self.state.font_size <= 0.0 || self.state.font_name.is_empty() {
            return Err(walk_error("Tj appeared before a valid Tf"));
        }
        let font = resolve_simple_font(self.document, &self.resources, &self.state.font_name)?;
        for byte in bytes {
            let transform = self.state.ctm.then(self.state.text_matrix);
            let baseline = transform.point(0.0, self.state.rise);
            let width = font.width(*byte);
            let word_spacing = if *byte == b' ' {
                self.state.word_spacing
            } else {
                0.0
            };
            let advance =
                (width * self.state.font_size / 1000.0 + self.state.char_spacing + word_spacing)
                    * self.state.horizontal_scale;
            let metric_box = transformed_box(
                transform,
                0.0,
                self.state.rise + font.descent * self.state.font_size / 1000.0,
                advance,
                self.state.rise + font.ascent * self.state.font_size / 1000.0,
            );
            self.characters.push(WalkedChar {
                unicode: decode_win_ansi(*byte),
                encoded: vec![*byte],
                font: font.reference.clone(),
                font_size: self.state.font_size,
                baseline_origin: baseline,
                metric_box,
                text_transform: classify_transform(transform),
                content_object: self.content_object,
                byte_start,
                byte_end,
            });
            self.state.text_matrix = self.state.text_matrix.translate(advance, 0.0);
        }
        Ok(())
    }
}

struct SimpleFont {
    reference: FontRef,
    first_char: i64,
    widths: Vec<f64>,
    ascent: f64,
    descent: f64,
}

impl SimpleFont {
    fn width(&self, code: u8) -> f64 {
        let index = i64::from(code) - self.first_char;
        usize::try_from(index)
            .ok()
            .and_then(|index| self.widths.get(index))
            .copied()
            .unwrap_or(0.0)
    }
}

fn resolve_simple_font(
    document: &Document,
    resources: &Dictionary,
    name: &[u8],
) -> Result<SimpleFont> {
    let fonts = resources
        .get_deref(b"Font", document)
        .and_then(Object::as_dict)
        .map_err(|error| walk_error(format!("page has no usable Font resources: {error}")))?;
    let font_object = fonts.get(name).map_err(|error| {
        walk_error(format!(
            "font /{} is missing: {error}",
            String::from_utf8_lossy(name)
        ))
    })?;
    let object_id = font_object
        .as_reference()
        .map_err(|_| walk_error("font resource must be an indirect object in the M1 path"))?;
    let font = document
        .get_object(object_id)
        .and_then(Object::as_dict)
        .map_err(|error| walk_error(format!("font object {} is invalid: {error}", object_id.0)))?;
    let first_char = font.get(b"FirstChar").and_then(Object::as_i64).unwrap_or(0);
    let widths = font
        .get(b"Widths")
        .and_then(Object::as_array)
        .map_err(|error| {
            walk_error(format!(
                "font object {} has no widths: {error}",
                object_id.0
            ))
        })?
        .iter()
        .map(object_number)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| walk_error("font Widths contains a non-number"))?;
    let descriptor = font
        .get_deref(b"FontDescriptor", document)
        .and_then(Object::as_dict)
        .map_err(|error| {
            walk_error(format!(
                "font object {} has no descriptor: {error}",
                object_id.0
            ))
        })?;
    let ascent = descriptor
        .get(b"Ascent")
        .ok()
        .and_then(object_number)
        .ok_or_else(|| walk_error("font descriptor has no numeric Ascent"))?;
    let descent = descriptor
        .get(b"Descent")
        .ok()
        .and_then(object_number)
        .ok_or_else(|| walk_error("font descriptor has no numeric Descent"))?;
    Ok(SimpleFont {
        reference: FontRef {
            resource_name: String::from_utf8_lossy(name).into_owned(),
            object_number: object_id.0,
            generation: object_id.1,
        },
        first_char,
        widths,
        ascent,
        descent,
    })
}

fn inherited_page_resources(document: &Document, page_id: ObjectId) -> Result<Dictionary> {
    let mut current = page_id;
    let mut visited = BTreeSet::new();
    for _ in 0..128 {
        if !visited.insert(current) {
            return Err(walk_error("page resource inheritance contains a cycle"));
        }
        let dictionary = document
            .get_object(current)
            .and_then(Object::as_dict)
            .map_err(|error| {
                walk_error(format!("invalid page tree object {}: {error}", current.0))
            })?;
        if let Ok(resources) = dictionary
            .get_deref(b"Resources", document)
            .and_then(Object::as_dict)
        {
            return Ok(resources.clone());
        }
        current = dictionary
            .get(b"Parent")
            .and_then(Object::as_reference)
            .map_err(|_| walk_error("page tree has no inherited Resources"))?;
    }
    Err(walk_error("page resource inheritance exceeds 128 levels"))
}

fn operand_tail<'a>(operands: &'a [Token], count: usize, operator: &str) -> Result<&'a [Token]> {
    if operands.len() != count {
        return Err(walk_error(format!(
            "{operator} requires exactly {count} operands, got {}",
            operands.len()
        )));
    }
    Ok(operands)
}

fn numeric_tail(operands: &[Token], count: usize, operator: &str) -> Result<Vec<f64>> {
    operand_tail(operands, count, operator)?
        .iter()
        .map(|token| match token.kind {
            TokenKind::Number(value) => Ok(value),
            _ => Err(walk_error(format!("{operator} requires numeric operands"))),
        })
        .collect()
}

fn object_number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn decode_win_ansi(code: u8) -> Option<char> {
    const C1: [u16; 32] = [
        0x20ac, 0, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039,
        0x0152, 0, 0x017d, 0, 0, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x02dc,
        0x2122, 0x0161, 0x203a, 0x0153, 0, 0x017e, 0x0178,
    ];
    match code {
        0x80..=0x9f => {
            let value = C1[usize::from(code - 0x80)];
            (value != 0)
                .then(|| char::from_u32(u32::from(value)))
                .flatten()
        }
        _ => char::from_u32(u32::from(code)),
    }
}

fn classify_transform(matrix: Matrix) -> TextTransform {
    let [a, b, c, d, _, _] = matrix.0;
    if a * d - b * c < 0.0 {
        return TextTransform::Mirrored;
    }
    let rotation = b.atan2(a).to_degrees();
    if rotation.abs() > 0.1 {
        return TextTransform::Rotated(rotation);
    }
    let skew = c.atan2(d).to_degrees().abs();
    if skew > 20.0 {
        TextTransform::Skewed(skew)
    } else {
        TextTransform::Upright
    }
}

fn transformed_box(matrix: Matrix, left: f64, bottom: f64, right: f64, top: f64) -> Rect {
    let points = [
        matrix.point(left, bottom),
        matrix.point(left, top),
        matrix.point(right, bottom),
        matrix.point(right, top),
    ];
    Rect {
        left: points
            .iter()
            .map(|point| point.x)
            .fold(f64::INFINITY, f64::min),
        bottom: points
            .iter()
            .map(|point| point.y)
            .fold(f64::INFINITY, f64::min),
        right: points
            .iter()
            .map(|point| point.x)
            .fold(f64::NEG_INFINITY, f64::max),
        top: points
            .iter()
            .map(|point| point.y)
            .fold(f64::NEG_INFINITY, f64::max),
    }
}

fn walk_error(message: impl Into<String>) -> MimusError {
    MimusError::input(ErrorReason::OperatorWalk, message)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf")
    }

    #[test]
    fn production_walk_reads_the_exact_single_line_without_the_poc() {
        let document = Document::load(fixture()).unwrap();
        let page_id = document.get_pages()[&1];
        let characters = walk_page(&document, page_id).unwrap();
        assert_eq!(
            characters
                .iter()
                .filter_map(|value| value.unicode)
                .collect::<String>(),
            "MIMUS"
        );
        assert_eq!(characters[0].baseline_origin, Point { x: 72.0, y: 120.0 });
        assert!((characters[1].baseline_origin.x - 82.356).abs() < 1e-9);
        assert_eq!(characters[0].font.object_number, 5);
        assert_eq!(characters[0].font.resource_name, "F1");
        assert_eq!(characters[0].text_transform, TextTransform::Upright);
        let metric = characters
            .iter()
            .skip(1)
            .fold(characters[0].metric_box, |bounds, character| {
                bounds.union(character.metric_box)
            });
        assert!((metric.left - 72.0).abs() < 1e-9);
        assert!((metric.bottom - 117.168).abs() < 1e-9);
        assert!((metric.right - 112.656).abs() < 1e-9);
        assert!((metric.top - 131.136).abs() < 1e-9);
    }
}
