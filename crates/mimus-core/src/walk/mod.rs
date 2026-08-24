mod tokenizer;

use std::collections::BTreeSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::error::{ErrorReason, InputReason, MimusError, Result};
use crate::event::{PageDegradeReason, RecoveryKind};
use crate::il::{FontRef, Point, Rect, TextTransform};
use tokenizer::{CompositeDelimiter, InlineImageLengthSource, Token, TokenKind, tokenize};

pub(crate) const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct WalkedChar {
    pub unicode: Option<char>,
    pub code: u32,
    pub visible: bool,
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
    character_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    leading: f64,
    rendering_mode: i32,
    rise: f64,
    phase: TextPhase,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            text_matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            font_name: Vec::new(),
            font_size: 0.0,
            character_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            rendering_mode: 0,
            rise: 0.0,
            phase: TextPhase::Outside,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextPhase {
    Outside,
    Inside,
}

struct Walker<'a> {
    document: &'a Document,
    resources: Dictionary,
    state: GraphicsState,
    operands: Vec<Token>,
    characters: Vec<WalkedChar>,
    content_object: ObjectId,
    recoveries: BTreeSet<RecoveryKind>,
    graphics_stack: Vec<GraphicsState>,
    compatibility_depth: usize,
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
    walk_page_detailed(document, page_id).map_err(PageWalkError::into_source)
}

#[derive(Debug)]
pub(crate) enum PageWalkError {
    Degraded {
        reason: PageDegradeReason,
        source: MimusError,
    },
    Fatal(MimusError),
}

impl PageWalkError {
    fn into_source(self) -> MimusError {
        match self {
            Self::Degraded { source, .. } | Self::Fatal(source) => source,
        }
    }
}

pub(crate) fn walk_page_detailed(
    document: &Document,
    page_id: ObjectId,
) -> std::result::Result<PageWalk, PageWalkError> {
    let resources = inherited_page_resources(document, page_id).map_err(PageWalkError::Fatal)?;
    let content_objects = document.get_page_contents(page_id);
    let mut walker = Walker {
        document,
        resources,
        state: GraphicsState::default(),
        operands: Vec::new(),
        characters: Vec::new(),
        content_object: (0, 0),
        recoveries: BTreeSet::new(),
        graphics_stack: Vec::new(),
        compatibility_depth: 0,
        text_object_is_implicit: false,
    };
    let mut content_streams = Vec::with_capacity(content_objects.len());
    for object_id in content_objects {
        let content = document.get_object(object_id).map_err(|error| {
            PageWalkError::Fatal(walk_error(format!(
                "page content {} is missing: {error}",
                object_id.0
            )))
        })?;
        let stream = content
            .as_stream()
            .map_err(|error| PageWalkError::Degraded {
                reason: PageDegradeReason::ContentDecode,
                source: walk_error(format!(
                    "page content {} is not a stream: {error}",
                    object_id.0
                )),
            })?;
        let decoded = stream
            .decompressed_content_with_limit(MAX_STREAM_BYTES)
            .map_err(|error| PageWalkError::Degraded {
                reason: PageDegradeReason::ContentDecode,
                source: walk_error(format!("could not decode content {}: {error}", object_id.0)),
            })?;
        walker.content_object = object_id;
        let tokens = tokenize(&decoded).map_err(|failure| PageWalkError::Degraded {
            reason: failure.reason,
            source: failure.into_mimus_error(),
        })?;
        walker.walk(tokens).map_err(classify_walk_error)?;
        content_streams.push(WalkedContentStream { object_id, decoded });
    }
    walker.finish();
    Ok(PageWalk {
        characters: walker.characters,
        recoveries: walker.recoveries,
        content_streams,
    })
}

fn classify_walk_error(error: MimusError) -> PageWalkError {
    if error.reason() == ErrorReason::Input(InputReason::OperatorWalk) {
        PageWalkError::Degraded {
            reason: PageDegradeReason::ContentStreamSyntax,
            source: error,
        }
    } else {
        PageWalkError::Fatal(error)
    }
}

impl Walker<'_> {
    fn walk(&mut self, tokens: Vec<Token>) -> Result<()> {
        for token in tokens {
            match &token.kind {
                TokenKind::InlineImage { length_source, .. } => {
                    if !self.operands.is_empty() {
                        self.recoveries.insert(RecoveryKind::ArityExcess);
                        self.operands.clear();
                    }
                    if *length_source == InlineImageLengthSource::EiScan {
                        self.recoveries.insert(RecoveryKind::InlineImageEiScan);
                    }
                }
                TokenKind::Operator(operator) => {
                    if let Some((first, second)) = split_double_decimal(operator) {
                        self.operands.push(number_token(first, token.span.clone()));
                        self.operands.push(number_token(second, token.span.clone()));
                        self.recoveries.insert(RecoveryKind::DoubleDecimal);
                        continue;
                    }
                    if let Some((number, recovered_operator)) = split_glued_operator(operator) {
                        self.operands.push(number_token(number, token.span.clone()));
                        let operands = std::mem::take(&mut self.operands);
                        self.apply_operator(recovered_operator, &operands)?;
                        self.recoveries.insert(RecoveryKind::GluedToken);
                        continue;
                    }
                    let operands = std::mem::take(&mut self.operands);
                    self.apply_operator(operator, &operands)?;
                }
                _ => self.operands.push(token),
            }
        }
        Ok(())
    }

    fn apply_operator(&mut self, operator: &[u8], operands: &[Token]) -> Result<()> {
        match operator {
            b"BX" => self.compatibility_depth = self.compatibility_depth.saturating_add(1),
            b"EX" => {
                if self.compatibility_depth == 0 {
                    self.recoveries.insert(RecoveryKind::CompatibilityUnderflow);
                } else {
                    self.compatibility_depth -= 1;
                }
            }
            b"q" => self.graphics_stack.push(self.state.clone()),
            b"Q" => self.restore_graphics_state(),
            b"cm" => {
                if let Some(values) = self.numeric_tail(operands, 6) {
                    self.state.ctm = self.state.ctm.then(Matrix::from_values(&values));
                }
            }
            b"BT" => self.begin_text_object(),
            b"ET" => self.end_text_object(),
            b"Tf" => {
                self.enter_text_phase();
                if let Some(tail) = self.operand_tail(operands, 2) {
                    if let (TokenKind::Name(name), TokenKind::Number(size)) =
                        (&tail[0].kind, &tail[1].kind)
                    {
                        self.state.font_name.clone_from(name);
                        self.state.font_size = *size;
                    } else {
                        self.recoveries.insert(RecoveryKind::InvalidOperands);
                    }
                }
            }
            b"Tm" => {
                self.enter_text_phase();
                if let Some(values) = self.numeric_tail(operands, 6) {
                    let matrix = Matrix::from_values(&values);
                    self.state.text_matrix = matrix;
                    self.state.line_matrix = matrix;
                }
            }
            b"Td" => self.move_text_position(operands, false),
            b"TD" => self.move_text_position(operands, true),
            b"T*" => {
                self.enter_text_phase();
                if self.operand_tail(operands, 0).is_some() {
                    self.state.line_matrix =
                        self.state.line_matrix.translate(0.0, -self.state.leading);
                    self.state.text_matrix = self.state.line_matrix;
                }
            }
            b"Tc" => self.set_text_number(operands, |state, value| {
                state.character_spacing = value;
            }),
            b"Tw" => self.set_text_number(operands, |state, value| {
                state.word_spacing = value;
            }),
            b"Tz" => self.set_text_number(operands, |state, value| {
                state.horizontal_scale = value / 100.0;
            }),
            b"TL" => self.set_text_number(operands, |state, value| {
                state.leading = value;
            }),
            b"Tr" => self.set_text_number(operands, |state, value| {
                state.rendering_mode = value as i32;
            }),
            b"Ts" => self.set_text_number(operands, |state, value| {
                state.rise = value;
            }),
            b"Tj" => {
                self.enter_text_phase();
                if let Some(tail) = self.operand_tail(operands, 1) {
                    if let TokenKind::Bytes(bytes) = &tail[0].kind {
                        self.show_text(bytes, tail[0].span.start, tail[0].span.end)?;
                    } else {
                        self.recoveries.insert(RecoveryKind::InvalidOperands);
                    }
                }
            }
            b"TJ" => {
                self.enter_text_phase();
                self.show_text_array(operands)?;
            }
            b"'" => {
                self.apply_operator(b"T*", &[])?;
                self.apply_operator(b"Tj", operands)?;
            }
            b"\"" => self.show_text_with_spacing(operands)?,
            _ if is_known_operator(operator) => {}
            _ if self.compatibility_depth > 0 => {}
            _ => {
                self.recoveries.insert(RecoveryKind::UnknownOperator);
            }
        }
        Ok(())
    }

    fn finish(&mut self) {
        if !self.operands.is_empty() {
            self.recoveries.insert(RecoveryKind::DanglingOperands);
            self.operands.clear();
        }
        if !self.graphics_stack.is_empty() {
            self.recoveries.insert(RecoveryKind::GraphicsStateUnclosed);
            self.graphics_stack.clear();
        }
        if self.compatibility_depth != 0 {
            self.recoveries.insert(RecoveryKind::CompatibilityUnclosed);
            self.compatibility_depth = 0;
        }
        if self.state.phase == TextPhase::Inside && !self.text_object_is_implicit {
            self.recoveries.insert(RecoveryKind::TextObjectUnclosed);
        }
        self.state.phase = TextPhase::Outside;
        self.text_object_is_implicit = false;
    }

    fn restore_graphics_state(&mut self) {
        let Some(mut restored) = self.graphics_stack.pop() else {
            self.recoveries.insert(RecoveryKind::GraphicsStateUnderflow);
            return;
        };
        // PDF q/Q 保存 graphics/text state 参数，但不保存 text/line matrix。
        restored.text_matrix = self.state.text_matrix;
        restored.line_matrix = self.state.line_matrix;
        restored.phase = self.state.phase;
        self.state = restored;
    }

    fn begin_text_object(&mut self) {
        if self.state.phase == TextPhase::Inside {
            self.recoveries.insert(RecoveryKind::NestedTextObject);
        }
        self.state.text_matrix = Matrix::IDENTITY;
        self.state.line_matrix = Matrix::IDENTITY;
        self.state.phase = TextPhase::Inside;
        self.text_object_is_implicit = false;
    }

    fn end_text_object(&mut self) {
        if self.state.phase == TextPhase::Outside {
            self.recoveries.insert(RecoveryKind::UnexpectedTextEnd);
            return;
        }
        self.state.phase = TextPhase::Outside;
        self.text_object_is_implicit = false;
    }

    fn move_text_position(&mut self, operands: &[Token], set_leading: bool) {
        self.enter_text_phase();
        if let Some(values) = self.numeric_tail(operands, 2) {
            if set_leading {
                self.state.leading = -values[1];
            }
            self.state.line_matrix = self.state.line_matrix.translate(values[0], values[1]);
            self.state.text_matrix = self.state.line_matrix;
        }
    }

    fn set_text_number(
        &mut self,
        operands: &[Token],
        update: impl FnOnce(&mut GraphicsState, f64),
    ) {
        self.enter_text_phase();
        if let Some(values) = self.numeric_tail(operands, 1) {
            update(&mut self.state, values[0]);
        }
    }

    fn show_text_with_spacing(&mut self, operands: &[Token]) -> Result<()> {
        self.enter_text_phase();
        let Some(tail) = self.operand_tail(operands, 3) else {
            return Ok(());
        };
        let (TokenKind::Number(word), TokenKind::Number(character), TokenKind::Bytes(bytes)) =
            (&tail[0].kind, &tail[1].kind, &tail[2].kind)
        else {
            self.recoveries.insert(RecoveryKind::InvalidOperands);
            return Ok(());
        };
        self.state.word_spacing = *word;
        self.state.character_spacing = *character;
        self.apply_operator(b"T*", &[])?;
        self.show_text(bytes, tail[2].span.start, tail[2].span.end)
    }

    fn operand_tail<'a>(&mut self, operands: &'a [Token], count: usize) -> Option<&'a [Token]> {
        if operands.len() < count {
            self.recoveries.insert(RecoveryKind::ArityShort);
            return None;
        }
        if operands.len() > count {
            self.recoveries.insert(RecoveryKind::ArityExcess);
        }
        Some(&operands[operands.len() - count..])
    }

    fn numeric_tail(&mut self, operands: &[Token], count: usize) -> Option<Vec<f64>> {
        let tail = self.operand_tail(operands, count)?;
        let values = tail
            .iter()
            .map(|token| match token.kind {
                TokenKind::Number(value) => Some(value),
                _ => None,
            })
            .collect::<Option<Vec<_>>>();
        if values.is_none() {
            self.recoveries.insert(RecoveryKind::InvalidOperands);
        }
        values
    }

    fn show_text(&mut self, bytes: &[u8], byte_start: usize, byte_end: usize) -> Result<()> {
        if self.state.font_name.is_empty() {
            return Err(walk_error("Tj appeared before Tf"));
        }
        let font = resolve_simple_font(self.document, &self.resources, &self.state.font_name)?;
        for byte in bytes {
            let transform = self.state.ctm.then(self.state.text_matrix);
            let baseline = transform.point(0.0, self.state.rise);
            let width = font.width(*byte).ok_or_else(|| {
                unsupported_error(format!(
                    "font /{} has no width for character code {byte}",
                    display_pdf_name(&self.state.font_name)
                ))
            })?;
            let glyph_width = width * self.state.font_size / 1000.0;
            let word_spacing = if *byte == b' ' {
                self.state.word_spacing
            } else {
                0.0
            };
            let advance = (glyph_width + self.state.character_spacing + word_spacing)
                * self.state.horizontal_scale;
            let metric_box = transformed_box(
                transform,
                0.0,
                font.descent * self.state.font_size / 1000.0 + self.state.rise,
                glyph_width * self.state.horizontal_scale,
                font.ascent * self.state.font_size / 1000.0 + self.state.rise,
            );
            self.characters.push(WalkedChar {
                unicode: decode_win_ansi(*byte),
                code: u32::from(*byte),
                visible: !matches!(self.state.rendering_mode, 3 | 7),
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

    /// STREAM-05：文本状态/显示操作符出现在 `BT` 外时按隐式 `BT` 处理。每次只在
    /// 页级集合里记录一种恢复，避免 warning 数随字符数量漂移。
    fn enter_text_phase(&mut self) {
        if self.state.phase == TextPhase::Outside {
            self.state.text_matrix = Matrix::IDENTITY;
            self.state.line_matrix = Matrix::IDENTITY;
            self.state.phase = TextPhase::Inside;
            self.text_object_is_implicit = true;
            self.recoveries.insert(RecoveryKind::ImplicitTextObject);
        }
    }

    /// STREAM-11：`TJ` 数组里合法的元素只有字符串与数字。别的类型跳过并记一次
    /// 恢复，字距按 0 计——规范没有给出别的可推断值，而整条 `TJ` 一起丢会连带
    /// 丢掉渲染器确实画出来的字符。
    fn show_text_array(&mut self, operands: &[Token]) -> Result<()> {
        let Some(last) = operands.last() else {
            self.recoveries.insert(RecoveryKind::ArityShort);
            return Ok(());
        };
        if !matches!(
            last.kind,
            TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayEnd)
        ) {
            self.recoveries.insert(RecoveryKind::InvalidOperands);
            return Ok(());
        }
        let mut depth = 0usize;
        let mut start = None;
        for (index, token) in operands.iter().enumerate().rev() {
            match token.kind {
                TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayEnd) => depth += 1,
                TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayStart) => {
                    depth -= 1;
                    if depth == 0 {
                        start = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(start) = start else {
            self.recoveries.insert(RecoveryKind::InvalidOperands);
            return Ok(());
        };
        if start != 0 {
            self.recoveries.insert(RecoveryKind::ArityExcess);
        }
        let elements = &operands[start + 1..operands.len() - 1];
        if elements.iter().any(|element| {
            matches!(
                element.kind,
                TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayStart)
                    | TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayEnd)
            )
        }) {
            self.recoveries.insert(RecoveryKind::SkippedTjElement);
            return Ok(());
        }
        for element in elements {
            match &element.kind {
                TokenKind::Bytes(bytes) => {
                    self.show_text(bytes, element.span.start, element.span.end)?;
                }
                TokenKind::Number(adjustment) => {
                    let shift =
                        -adjustment * self.state.font_size / 1000.0 * self.state.horizontal_scale;
                    self.state.text_matrix = self.state.text_matrix.translate(shift, 0.0);
                }
                TokenKind::Name(_)
                | TokenKind::Operator(_)
                | TokenKind::InlineImage { .. }
                | TokenKind::CompositeDelimiter(_) => {
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

fn number_token(value: f64, span: std::ops::Range<usize>) -> Token {
    Token {
        kind: TokenKind::Number(value),
        span,
    }
}

fn split_double_decimal(operator: &[u8]) -> Option<(f64, f64)> {
    let dots = operator
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'.').then_some(index))
        .collect::<Vec<_>>();
    let [_, split] = dots.as_slice() else {
        return None;
    };
    let first = std::str::from_utf8(&operator[..*split])
        .ok()?
        .parse::<f64>()
        .ok()?;
    let second = std::str::from_utf8(&operator[*split..])
        .ok()?
        .parse::<f64>()
        .ok()?;
    (first.is_finite() && second.is_finite()).then_some((first, second))
}

fn split_glued_operator(value: &[u8]) -> Option<(f64, &'static [u8])> {
    const SUFFIXES: [&[u8]; 8] = [b"Tm", b"Td", b"Tf", b"Tc", b"Tw", b"Tz", b"Ts", b"cm"];
    SUFFIXES.iter().find_map(|suffix| {
        if !value.ends_with(suffix) {
            return None;
        }
        let prefix = &value[..value.len() - suffix.len()];
        if prefix.is_empty() {
            return None;
        }
        let number = std::str::from_utf8(prefix).ok()?.parse::<f64>().ok()?;
        number.is_finite().then_some((number, *suffix))
    })
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
            | b"Do"
            | b"d0"
            | b"d1"
    )
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

    fn walk_fixture(id: &str) -> PageWalk {
        let document = Document::load(fixture_path(id)).unwrap();
        let page_id = document.get_pages()[&1];
        walk_page(&document, page_id)
            .unwrap_or_else(|error| panic!("fixture {id} should be walked: {error}"))
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
    fn production_walk_keeps_contents_streams_separate_and_carries_state_between_them() {
        let id = "unit-parse-04-contents-array-numeric-split";
        let document = Document::load(fixture_path(id)).unwrap();
        let page_id = document.get_pages()[&1];
        let walked = walk_page(&document, page_id).unwrap();

        assert_eq!(text_of(&walked), "MIMUS");
        assert_eq!(
            walked.characters[0].baseline_origin,
            Point { x: 82.0, y: 140.0 }
        );
        assert_eq!(
            walked
                .content_streams
                .iter()
                .map(|stream| stream.object_id)
                .collect::<Vec<_>>(),
            vec![(9, 0), (10, 0)]
        );
        assert!(walked.recoveries.is_empty());
    }

    #[test]
    fn production_walk_matches_the_stream_recovery_fixture_matrix() {
        let cases: &[(&str, Point, &[RecoveryKind])] = &[
            (
                "mal-stream-03-arity-excess",
                Point { x: 630.0, y: 823.0 },
                &[RecoveryKind::ArityExcess],
            ),
            (
                "mal-stream-04-arity-short",
                Point { x: 72.0, y: 120.0 },
                &[RecoveryKind::ArityShort],
            ),
            (
                "mal-stream-05-unbalanced-Q",
                Point { x: 72.0, y: 120.0 },
                &[RecoveryKind::GraphicsStateUnderflow],
            ),
            (
                "mal-stream-06-glued-tokens",
                Point { x: 100.0, y: 120.0 },
                &[RecoveryKind::GluedToken],
            ),
            (
                "mal-stream-07-double-decimal",
                Point { x: 72.0, y: 120.0 },
                &[RecoveryKind::ArityExcess, RecoveryKind::DoubleDecimal],
            ),
            (
                "mal-stream-08-unknown-outside-bx",
                Point { x: 72.0, y: 120.0 },
                &[
                    RecoveryKind::UnknownOperator,
                    RecoveryKind::CompatibilityUnderflow,
                ],
            ),
            (
                "mal-stream-09-orphan-text",
                Point { x: 72.0, y: 120.0 },
                &[RecoveryKind::ImplicitTextObject],
            ),
        ];

        for (id, baseline, recoveries) in cases {
            let walked = walk_fixture(id);
            assert_eq!(text_of(&walked), "MIMUS", "fixture {id}");
            assert_eq!(
                walked.characters[0].baseline_origin, *baseline,
                "fixture {id}"
            );
            assert_eq!(
                walked.recoveries,
                recoveries.iter().copied().collect(),
                "fixture {id}"
            );
        }

        for id in [
            "unit-stream-01-bx-ex-unknown-op",
            "unit-stream-03-unknown-op-outside-bx",
        ] {
            let walked = walk_fixture(id);
            assert_eq!(text_of(&walked), "MIMUS", "fixture {id}");
            assert!(walked.recoveries.is_empty(), "fixture {id}");
        }
    }

    #[test]
    fn production_walk_uses_all_three_bounded_inline_image_length_paths() {
        for (id, recoveries) in [
            ("unit-stream-08-inline-image-EI-in-data", &[][..]),
            ("unit-stream-09-inline-image-no-L", &[][..]),
            ("unit-stream-10-inline-image-length", &[][..]),
            (
                "unit-stream-11-inline-image-filtered-fallback",
                &[RecoveryKind::InlineImageEiScan][..],
            ),
        ] {
            let walked = walk_fixture(id);
            assert_eq!(text_of(&walked), "MIMUS", "fixture {id}");
            assert_eq!(
                walked.characters[0].baseline_origin,
                Point { x: 72.0, y: 120.0 },
                "fixture {id}"
            );
            assert_eq!(
                walked.recoveries,
                recoveries.iter().copied().collect(),
                "fixture {id}"
            );
        }
    }

    #[test]
    fn detailed_walk_failures_distinguish_syntax_nesting_and_decode_degradation() {
        for (id, expected) in [
            (
                "mal-stream-10-unterminated-string",
                PageDegradeReason::ContentStreamSyntax,
            ),
            (
                "mal-parse-06-deep-nesting",
                PageDegradeReason::NestingTooDeep,
            ),
        ] {
            let document = Document::load(fixture_path(id)).unwrap();
            let page_id = document.get_pages()[&1];
            let error = walk_page_detailed(&document, page_id).unwrap_err();
            assert!(
                matches!(error, PageWalkError::Degraded { reason, .. } if reason == expected),
                "fixture {id}"
            );
        }

        let mut document = Document::load(fixture()).unwrap();
        document
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .dict
            .set("Filter", Object::Name(b"UnsupportedFilter".to_vec()));
        let page_id = document.get_pages()[&1];
        let error = walk_page_detailed(&document, page_id).unwrap_err();
        assert!(matches!(
            error,
            PageWalkError::Degraded {
                reason: PageDegradeReason::ContentDecode,
                ..
            }
        ));
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
    fn production_walk_retains_non_positive_tf_for_later_font_reliability_checks() {
        for size in ["-12", "0"] {
            let walked = walk_program(
                format!("BT /F1 {size} Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET").as_bytes(),
            )
            .unwrap();
            assert_eq!(text_of(&walked), "MIMUS");
            assert!(
                walked
                    .characters
                    .iter()
                    .all(|character| character.font_size <= 0.0)
            );
            assert!(walked.recoveries.is_empty());
        }
    }

    #[test]
    fn production_walk_attributes_composite_operands_to_their_operator() {
        let marked =
            walk_program(b"/Span <</MCID 0>> BDC BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET EMC")
                .unwrap();
        assert_eq!(text_of(&marked), "MIMUS");
        assert!(marked.recoveries.is_empty());

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

    /// 缺失 `ET` 不会抹掉此前已经完整产出的字符，但恢复决定必须按页报告。
    #[test]
    fn an_explicit_text_object_without_et_is_recovered_at_page_end() {
        let walked = walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj").unwrap();
        assert_eq!(text_of(&walked), "MIMUS");
        assert_eq!(
            walked.recoveries,
            BTreeSet::from([RecoveryKind::TextObjectUnclosed])
        );
    }

    #[test]
    fn production_walk_tracks_graphics_text_state_and_multiple_text_shows() {
        let programs: &[(&[u8], &str)] = &[
            (
                b"0.5 0 0 0.5 0 0 cm\nBT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
                "MIMUS",
            ),
            (
                b"BT /F1 12 Tf 2 Tc 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
                "MIMUS",
            ),
            (
                b"BT /F1 12 Tf 2 Tw 1 0 0 1 72 120 Tm (MI MUS) Tj ET",
                "MI MUS",
            ),
            (
                b"BT /F1 12 Tf 50 Tz 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
                "MIMUS",
            ),
            (
                b"BT /F1 12 Tf 2 Ts 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
                "MIMUS",
            ),
            (
                b"BT /F1 12 Tf 3 Tr 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
                "MIMUS",
            ),
            (b"BT /F1 12 Tf 0.5 0 0 0.5 72 120 Tm (MIMUS) Tj ET", "MIMUS"),
            (
                b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj 1 0 0 1 72 80 Tm (MIMUS) Tj ET",
                "MIMUSMIMUS",
            ),
            (
                b"0 0 10 10 re f BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET",
                "MIMUS",
            ),
        ];
        for (program, expected_text) in programs {
            let walked = walk_program(program).unwrap();
            assert_eq!(text_of(&walked), *expected_text);
            assert!(walked.recoveries.is_empty(), "program {program:?}");
        }

        let scaled = walk_program(programs[0].0).unwrap();
        assert_eq!(
            scaled.characters[0].baseline_origin,
            Point { x: 36.0, y: 60.0 }
        );

        let hidden = walk_program(programs[5].0).unwrap();
        assert!(hidden.characters.iter().all(|character| !character.visible));

        let multiple = walk_program(programs[7].0).unwrap();
        assert_eq!(multiple.characters.len(), 10);
        assert_eq!(multiple.characters[0].baseline_origin.y, 120.0);
        assert_eq!(multiple.characters[5].baseline_origin.y, 80.0);
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
