mod tokenizer;

use std::collections::BTreeSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::error::{InputReason, MimusError, Result};
use crate::event::RecoveryKind;
use crate::il::{FontRef, Point, Rect, TextTransform};
use tokenizer::{Token, TokenKind, tokenize};

pub(crate) const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct WalkedChar {
    pub unicode: Option<char>,
    pub code: u32,
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

    fn has_identity_linear_part(self) -> bool {
        let [a, b, c, d, _, _] = self.0;
        (a - 1.0).abs() <= f64::EPSILON
            && b.abs() <= f64::EPSILON
            && c.abs() <= f64::EPSILON
            && (d - 1.0).abs() <= f64::EPSILON
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
    text_matrix: Matrix,
    font_name: Vec<u8>,
    font_size: f64,
    phase: TextPhase,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            text_matrix: Matrix::IDENTITY,
            font_name: Vec::new(),
            font_size: 0.0,
            phase: TextPhase::Before,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextPhase {
    Before,
    Inside,
    After,
}

struct Walker<'a> {
    document: &'a Document,
    resources: Dictionary,
    state: GraphicsState,
    operands: Vec<Token>,
    characters: Vec<WalkedChar>,
    content_object: ObjectId,
    text_show_count: usize,
    recoveries: BTreeSet<RecoveryKind>,
    text_object_is_implicit: bool,
}

/// 一页走查的结果。`recoveries` 用集合而非计数：ADR-0013 §3 要求恢复决定
/// **每页一致**，所以消费者要知道的是「这一页用过哪几类恢复」，
/// 而不是「用过多少次」——后者会随内容长度漂移，做不成稳定断言。
#[derive(Debug, Clone, PartialEq)]
pub struct PageWalk {
    pub characters: Vec<WalkedChar>,
    pub recoveries: BTreeSet<RecoveryKind>,
    pub(crate) content_streams: Vec<WalkedContentStream>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WalkedContentStream {
    pub object_id: ObjectId,
    pub decoded: Vec<u8>,
}

pub fn walk_page(document: &Document, page_id: ObjectId) -> Result<PageWalk> {
    let resources = inherited_page_resources(document, page_id)?;
    let content_objects = document.get_page_contents(page_id);
    if content_objects.len() > 1 {
        return Err(unsupported_error(
            "M1 supports exactly one content stream per page",
        ));
    }
    let mut walker = Walker {
        document,
        resources,
        state: GraphicsState::default(),
        operands: Vec::new(),
        characters: Vec::new(),
        content_object: (0, 0),
        text_show_count: 0,
        recoveries: BTreeSet::new(),
        text_object_is_implicit: false,
    };
    let mut content_streams = Vec::with_capacity(content_objects.len());
    for object_id in content_objects {
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
        content_streams.push(WalkedContentStream { object_id, decoded });
    }
    // 显式 `BT` 没等到 `ET` 是流被截断的信号，仍然报错；隐式打开的文本对象本来就
    // 没有对应的 `ET`，在流尾隐式闭合。
    if walker.state.phase == TextPhase::Inside && !walker.text_object_is_implicit {
        return Err(walk_error("content stream ended before ET"));
    }
    Ok(PageWalk {
        characters: walker.characters,
        recoveries: walker.recoveries,
        content_streams,
    })
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
                b"BT" => {
                    operand_tail(&operands, 0, "BT")?;
                    if self.state.phase != TextPhase::Before {
                        return Err(unsupported_error(
                            "M1 supports exactly one text object per page",
                        ));
                    }
                    self.state.text_matrix = Matrix::IDENTITY;
                    self.state.phase = TextPhase::Inside;
                }
                b"ET" => {
                    operand_tail(&operands, 0, "ET")?;
                    if self.state.phase != TextPhase::Inside {
                        return Err(walk_error("ET appeared outside a text object"));
                    }
                    if self.text_show_count != 1 {
                        return Err(unsupported_error(
                            "M1 requires exactly one text-show operation per page",
                        ));
                    }
                    self.state.phase = TextPhase::After;
                }
                b"Tf" => {
                    self.enter_text_phase("Tf")?;
                    let tail = operand_tail(&operands, 2, "Tf")?;
                    let TokenKind::Name(name) = &tail[0].kind else {
                        return Err(walk_error("Tf font operand is not a name"));
                    };
                    let TokenKind::Number(size) = tail[1].kind else {
                        return Err(walk_error("Tf size operand is not a number"));
                    };
                    if size <= 0.0 {
                        return Err(unsupported_error(format!(
                            "M1 cannot faithfully re-emit non-positive Tf font size {size}"
                        )));
                    }
                    self.state.font_name.clone_from(name);
                    self.state.font_size = size;
                }
                b"Tm" => {
                    self.enter_text_phase("Tm")?;
                    let values = numeric_tail(&operands, 6, "Tm")?;
                    let matrix = Matrix::from_values(&values);
                    if !matrix.has_identity_linear_part() {
                        return Err(unsupported_error(
                            "M1 cannot faithfully re-emit scaled, rotated, mirrored, or skewed text matrices",
                        ));
                    }
                    self.state.text_matrix = matrix;
                }
                b"Tj" => {
                    self.enter_text_phase("Tj")?;
                    self.begin_text_show()?;
                    let tail = operand_tail(&operands, 1, "Tj")?;
                    let TokenKind::Bytes(bytes) = &tail[0].kind else {
                        return Err(walk_error("Tj operand is not a string"));
                    };
                    self.show_text(bytes, tail[0].span.start, tail[0].span.end)?;
                }
                b"TJ" => {
                    self.enter_text_phase("TJ")?;
                    self.begin_text_show()?;
                    self.show_text_array(&operands)?;
                }
                _ => {
                    return Err(unsupported_error(format!(
                        "M1 cannot faithfully re-emit operator {}",
                        display_operator(operator)
                    )));
                }
            }
        }
        if !self.operands.is_empty() {
            return Err(walk_error("content stream ended with unused operands"));
        }
        Ok(())
    }

    fn show_text(&mut self, bytes: &[u8], byte_start: usize, byte_end: usize) -> Result<()> {
        if self.state.font_name.is_empty() {
            return Err(walk_error("Tj appeared before Tf"));
        }
        let font = resolve_simple_font(self.document, &self.resources, &self.state.font_name)?;
        for byte in bytes {
            let transform = self.state.text_matrix;
            let baseline = transform.point(0.0, 0.0);
            let width = font.width(*byte).ok_or_else(|| {
                unsupported_error(format!(
                    "font /{} has no width for character code {byte}",
                    display_pdf_name(&self.state.font_name)
                ))
            })?;
            let glyph_width = width * self.state.font_size / 1000.0;
            let metric_box = transformed_box(
                transform,
                0.0,
                font.descent * self.state.font_size / 1000.0,
                glyph_width,
                font.ascent * self.state.font_size / 1000.0,
            );
            self.characters.push(WalkedChar {
                unicode: decode_win_ansi(*byte),
                code: u32::from(*byte),
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
            self.state.text_matrix = self.state.text_matrix.translate(glyph_width, 0.0);
        }
        Ok(())
    }

    /// STREAM-05：`BT` 之前出现的文本操作符按隐式 `BT` 处理——渲染器确实会画出
    /// 这些字，丢掉它们就是丢正文。`ET` 之后再出现则是第二个文本对象，
    /// 与 M1 的单文本对象边界撞车，仍然拒绝。
    fn enter_text_phase(&mut self, operator: &str) -> Result<()> {
        match self.state.phase {
            TextPhase::Inside => Ok(()),
            TextPhase::Before => {
                self.state.text_matrix = Matrix::IDENTITY;
                self.state.phase = TextPhase::Inside;
                self.text_object_is_implicit = true;
                self.recoveries.insert(RecoveryKind::ImplicitTextObject);
                Ok(())
            }
            TextPhase::After => Err(unsupported_error(format!(
                "M1 supports exactly one text object per page; {operator} appeared after ET"
            ))),
        }
    }

    fn begin_text_show(&mut self) -> Result<()> {
        if self.text_show_count != 0 {
            return Err(unsupported_error(
                "M1 supports exactly one text-show operation per page",
            ));
        }
        self.text_show_count += 1;
        Ok(())
    }

    /// STREAM-11：`TJ` 数组里合法的元素只有字符串与数字。别的类型跳过并记一次
    /// 恢复，字距按 0 计——规范没有给出别的可推断值，而整条 `TJ` 一起丢会连带
    /// 丢掉渲染器确实画出来的字符。
    fn show_text_array(&mut self, operands: &[Token]) -> Result<()> {
        let (Some(first), Some(last)) = (operands.first(), operands.last()) else {
            return Err(walk_error("TJ requires an array operand"));
        };
        if !matches!(first.kind, TokenKind::CompositeDelimiter)
            || !matches!(last.kind, TokenKind::CompositeDelimiter)
            || operands.len() < 2
        {
            return Err(walk_error("TJ operand is not a bracketed array"));
        }
        for element in &operands[1..operands.len() - 1] {
            match &element.kind {
                TokenKind::Bytes(bytes) => {
                    self.show_text(bytes, element.span.start, element.span.end)?;
                }
                TokenKind::Number(adjustment) => {
                    let shift = -adjustment * self.state.font_size / 1000.0;
                    self.state.text_matrix = self.state.text_matrix.translate(shift, 0.0);
                }
                // 扁平 token 流里嵌套数组只表现为一个多余的分隔符。把它当成可跳过的
                // 元素会把内层的数字悄悄当成字距，位置就错了——那不是恢复，是编造。
                TokenKind::CompositeDelimiter => {
                    return Err(unsupported_error(
                        "M1 does not yet handle arrays nested inside TJ",
                    ));
                }
                TokenKind::Name(_) | TokenKind::Operator(_) => {
                    self.recoveries.insert(RecoveryKind::SkippedTjElement);
                }
            }
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
    fn width(&self, code: u8) -> Option<f64> {
        let index = i64::from(code) - self.first_char;
        usize::try_from(index)
            .ok()
            .and_then(|index| self.widths.get(index))
            .copied()
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
            display_pdf_name(name)
        ))
    })?;
    let object_id = font_object
        .as_reference()
        .map_err(|_| walk_error("font resource must be an indirect object in the M1 path"))?;
    let font = document
        .get_object(object_id)
        .and_then(Object::as_dict)
        .map_err(|error| walk_error(format!("font object {} is invalid: {error}", object_id.0)))?;
    let subtype = font
        .get(b"Subtype")
        .and_then(Object::as_name)
        .map_err(|error| {
            walk_error(format!(
                "font object {} has no valid Subtype: {error}",
                object_id.0
            ))
        })?;
    if !matches!(subtype, b"Type1" | b"TrueType") {
        return Err(unsupported_error(format!(
            "M1 supports only Type1 and TrueType simple fonts with explicit metrics; font /{} uses /Subtype /{}",
            display_pdf_name(name),
            display_pdf_name(subtype)
        )));
    }
    // Standard 14 字体合法省略 FontDescriptor；缺少显式度量是能力边界，不是内容流语法错误。
    if subtype == b"Type1" && !font.has(b"FontDescriptor") {
        if let Ok(base_font) = font.get(b"BaseFont").and_then(Object::as_name) {
            if is_standard_14_name(base_font) {
                return Err(unsupported_error(format!(
                    "M1 requires explicit FontDescriptor metrics; font /{} uses Standard 14 /{}",
                    display_pdf_name(name),
                    display_pdf_name(base_font)
                )));
            }
        }
    }
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
            resource_name: String::from_utf8(name.to_vec()).map_err(|_| {
                unsupported_error(format!(
                    "M1 cannot faithfully re-emit non-UTF-8 font resource /{}",
                    display_pdf_name(name)
                ))
            })?,
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

fn is_standard_14_name(name: &[u8]) -> bool {
    const NAMES: [&[u8]; 14] = [
        b"Times-Roman",
        b"Times-Bold",
        b"Times-Italic",
        b"Times-BoldItalic",
        b"Helvetica",
        b"Helvetica-Bold",
        b"Helvetica-Oblique",
        b"Helvetica-BoldOblique",
        b"Courier",
        b"Courier-Bold",
        b"Courier-Oblique",
        b"Courier-BoldOblique",
        b"Symbol",
        b"ZapfDingbats",
    ];
    NAMES.contains(&name)
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
    // CONTEXT #32 / ADR-0007: 直立窗口只有 0deg +/-0.1deg，镜像优先于旋转，
    // 斜切超过 20deg 才隔离；页面 /Rotate 在 Parse 中另行处理，不能混入这里。
    const UPRIGHT_TOLERANCE_DEGREES: f64 = 0.1;
    const MAX_UPRIGHT_SKEW_DEGREES: f64 = 20.0;
    let [a, b, c, d, _, _] = matrix.0;
    if a * d - b * c < 0.0 {
        return TextTransform::Mirrored;
    }
    let rotation = b.atan2(a).to_degrees();
    if rotation.abs() > UPRIGHT_TOLERANCE_DEGREES {
        return TextTransform::Rotated(rotation);
    }
    let skew = c.atan2(d).to_degrees().abs();
    if skew > MAX_UPRIGHT_SKEW_DEGREES {
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
    MimusError::input(InputReason::OperatorWalk, message)
}

fn unsupported_error(message: impl Into<String>) -> MimusError {
    MimusError::input(InputReason::UnsupportedPdf, message)
}

fn display_operator(operator: &[u8]) -> String {
    operator
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() {
                char::from(*byte).to_string()
            } else {
                format!("\\x{byte:02X}")
            }
        })
        .collect()
}

fn display_pdf_name(name: &[u8]) -> String {
    name.iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                char::from(*byte).to_string()
            } else {
                format!("#{byte:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::error::ErrorReason;

    use super::*;

    fn fixture_path(id: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures")
            .join(id)
            .join(format!("{id}.pdf"))
    }

    fn fixture() -> PathBuf {
        fixture_path("unit-base-01-single-line")
    }

    /// 把一段 content stream 装进钉死的单行 fixture 再走查。用同一份字体与页面，
    /// 所以两段程序的走查结果可以逐字符直接比较。
    fn walk_program(program: &[u8]) -> Result<PageWalk> {
        let mut document = Document::load(fixture()).unwrap();
        document
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(program.to_vec());
        let page_id = document.get_pages()[&1];
        walk_page(&document, page_id)
    }

    fn text_of(walk: &PageWalk) -> String {
        walk.characters
            .iter()
            .filter_map(|character| character.unicode)
            .collect()
    }

    #[test]
    fn production_walk_reads_the_exact_single_line_without_the_poc() {
        let document = Document::load(fixture()).unwrap();
        let page_id = document.get_pages()[&1];
        let characters = walk_page(&document, page_id).unwrap().characters;
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
        assert_eq!(characters[0].code, u32::from(b'M'));
    }

    #[test]
    fn production_walk_accepts_only_safe_experiment_2_decoding_variants() {
        for id in [
            "unit-parse-01-ascii85",
            "unit-parse-02-cascade",
            "unit-parse-03-lzw-earlychange",
            "unit-parse-03-lzw-earlychange-1",
            "unit-parse-07-inherited-page-resources",
        ] {
            let document = Document::load(fixture_path(id)).unwrap();
            let page_id = document.get_pages()[&1];
            let text = walk_page(&document, page_id)
                .unwrap_or_else(|error| panic!("{id} should be accepted: {error}"))
                .characters
                .iter()
                .filter_map(|character| character.unicode)
                .collect::<String>();
            assert_eq!(text, "MIMUS", "fixture {id}");
        }
    }

    #[test]
    fn production_walk_rejects_experiment_2_content_it_cannot_reemit() {
        for id in [
            "unit-parse-04-contents-array-numeric-split",
            "unit-stream-01-bx-ex-unknown-op",
            "unit-stream-02-type3-d1",
            "unit-stream-03-unknown-op-outside-bx",
            "unit-stream-08-inline-image-EI-in-data",
            "unit-xobj-00-recursion-parent",
            "unit-xobj-04-inherited-resources",
        ] {
            let document = Document::load(fixture_path(id)).unwrap();
            let page_id = document.get_pages()[&1];
            let result = walk_page(&document, page_id);
            assert!(result.is_err(), "{id} must fail closed");
            let error = result.unwrap_err();
            assert_eq!(error.category().code(), 2, "fixture {id}: {error}");
        }
    }

    #[test]
    fn production_walk_classifies_legal_out_of_scope_fonts_as_unsupported() {
        for (id, expected_message) in [
            ("unit-cmap-01-identity-no-tounicode", "Subtype /Type0"),
            ("unit-stream-02-type3-d1", "Subtype /Type3"),
            ("unit-font-01-std14-custom-widths", "Standard 14 /Helvetica"),
        ] {
            let document = Document::load(fixture_path(id)).unwrap();
            let page_id = document.get_pages()[&1];
            let error = walk_page(&document, page_id).unwrap_err();
            assert_eq!(
                error.reason(),
                ErrorReason::Input(InputReason::UnsupportedPdf),
                "fixture {id}: {error}"
            );
            assert!(
                error.to_string().contains(expected_message),
                "fixture {id}: {error}"
            );
        }
    }

    #[test]
    fn production_walk_classifies_non_positive_tf_as_unsupported() {
        for size in ["-12", "0"] {
            let mut document = Document::load(fixture()).unwrap();
            document
                .get_object_mut((9, 0))
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .set_plain_content(
                    format!("BT /F1 {size} Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET").into_bytes(),
                );
            let page_id = document.get_pages()[&1];
            let error = walk_page(&document, page_id).unwrap_err();
            assert_eq!(
                error.reason(),
                ErrorReason::Input(InputReason::UnsupportedPdf)
            );
            assert!(
                error.to_string().contains("non-positive Tf font size"),
                "Tf size {size}: {error}"
            );
        }
    }

    #[test]
    fn production_walk_attributes_composite_operands_to_their_operator() {
        for id in ["unit-base-02-two-column", "unit-form-01-display"] {
            let document = Document::load(fixture_path(id)).unwrap();
            let page_id = document.get_pages()[&1];
            let error = walk_page(&document, page_id).unwrap_err();
            assert_eq!(
                error.reason(),
                ErrorReason::Input(InputReason::UnsupportedPdf)
            );
            assert!(
                error.to_string().contains("operator BDC"),
                "fixture {id}: {error}"
            );
            assert!(!error.to_string().contains("operator <<"));
        }

        // `[` 与 `]` 是操作数分隔符，不是操作符——`TJ` 必须拿到它们之间的元素。
        let walked = walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm [(MIMUS)] TJ ET").unwrap();
        assert_eq!(text_of(&walked), "MIMUS");
        assert_eq!(
            walked.characters[0].baseline_origin,
            Point { x: 72.0, y: 120.0 }
        );
        assert!(walked.recoveries.is_empty());
    }

    /// STREAM-11。`/X` 被跳过且字距按 0 计，所以 `MIM` 与 `US` 仍然首尾相接——
    /// 与同一行写成一个字符串时逐字符同位。
    #[test]
    fn production_walk_skips_an_illegal_tj_element_without_moving_the_rest() {
        let recovered =
            walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm [(MIM) /X (US)] TJ ET").unwrap();
        let intact = walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET").unwrap();

        assert_eq!(text_of(&recovered), "MIMUS");
        assert_eq!(
            recovered.recoveries,
            BTreeSet::from([RecoveryKind::SkippedTjElement])
        );
        let origins = |walk: &PageWalk| {
            walk.characters
                .iter()
                .map(|character| character.baseline_origin)
                .collect::<Vec<_>>()
        };
        assert_eq!(origins(&recovered), origins(&intact));
    }

    /// STREAM-11 的字距元素本身仍要生效：`TJ` 里的数字按 -value/1000 × 字号左移。
    #[test]
    fn production_walk_applies_tj_kerning() {
        let kerned = walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm [(MI) -1000 (MUS)] TJ ET")
            .unwrap()
            .characters;
        let intact = walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET")
            .unwrap()
            .characters;

        assert_eq!(kerned.len(), intact.len());
        for index in 0..2 {
            assert_eq!(kerned[index].baseline_origin, intact[index].baseline_origin);
        }
        for index in 2..5 {
            let shift = kerned[index].baseline_origin.x - intact[index].baseline_origin.x;
            assert!((shift - 12.0).abs() < 1e-9, "character {index}: {shift}");
        }
    }

    /// STREAM-05。`BT` 之前的文本操作符按隐式文本对象处理，字符位置与显式写法
    /// 完全一致，并且恢复这件事必须被报告出来。
    #[test]
    fn production_walk_recovers_text_operators_outside_bt_et() {
        let orphan = walk_program(b"/F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj\n").unwrap();
        let intact = walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET").unwrap();

        assert_eq!(text_of(&orphan), "MIMUS");
        // 字节跨度当然不同——少了 `BT `，字符串在流里的位置就前移了。
        // 需要一致的是几何：隐式文本对象的 `Tm` 与显式写法同为单位阵起点。
        for (recovered, expected) in orphan.characters.iter().zip(&intact.characters) {
            assert_eq!(recovered.baseline_origin, expected.baseline_origin);
            assert_eq!(recovered.metric_box, expected.metric_box);
            assert_eq!(recovered.text_transform, expected.text_transform);
        }
        assert_eq!(
            orphan.recoveries,
            BTreeSet::from([RecoveryKind::ImplicitTextObject])
        );
        assert!(intact.recoveries.is_empty());
    }

    /// 隐式 `BT` 在流尾隐式闭合，但显式 `BT` 少了 `ET` 依旧是截断信号。
    #[test]
    fn an_explicit_text_object_still_needs_its_et() {
        let error = walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj").unwrap_err();
        assert_eq!(
            error.reason(),
            ErrorReason::Input(InputReason::OperatorWalk)
        );
        assert!(error.to_string().contains("ended before ET"), "{error}");
    }

    #[test]
    fn production_walk_rejects_unreplayed_state_and_multiple_text_shows() {
        let programs: &[&[u8]] = &[
            b"0.5 0 0 0.5 0 0 cm\nBT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
            b"BT /F1 12 Tf 2 Tc 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
            b"BT /F1 12 Tf 2 Tw 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
            b"BT /F1 12 Tf 50 Tz 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
            b"BT /F1 12 Tf 2 Ts 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
            b"BT /F1 12 Tf 3 Tr 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
            b"BT /F1 12 Tf 0.5 0 0 0.5 72 120 Tm (MIMUS) Tj ET",
            b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj 1 0 0 1 72 80 Tm (MIMUS) Tj ET",
            b"0 0 10 10 re f BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
        ];
        for program in programs {
            let mut document = Document::load(fixture()).unwrap();
            document
                .get_object_mut((9, 0))
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .set_plain_content(program.to_vec());
            let page_id = document.get_pages()[&1];
            let error = walk_page(&document, page_id).unwrap_err();
            assert_eq!(
                error.reason(),
                ErrorReason::Input(InputReason::UnsupportedPdf)
            );
        }
    }

    #[test]
    fn production_walk_rejects_a_font_name_it_cannot_reemit_losslessly() {
        let mut document = Document::load(fixture()).unwrap();
        let resources = document
            .get_object_mut((4, 0))
            .unwrap()
            .as_dict_mut()
            .unwrap();
        let fonts = resources.get_mut(b"Font").unwrap().as_dict_mut().unwrap();
        let font = fonts.remove(b"F1").unwrap();
        fonts.set(vec![0xff], font);
        document
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(b"BT /#FF 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET".to_vec());

        let page_id = document.get_pages()[&1];
        let error = walk_page(&document, page_id).unwrap_err();
        assert_eq!(
            error.reason(),
            ErrorReason::Input(InputReason::UnsupportedPdf)
        );
    }
}
