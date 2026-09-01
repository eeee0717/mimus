mod font;
mod tokenizer;

use std::collections::BTreeSet;
use std::ops::Range;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::error::{ErrorReason, InputReason, MimusError, Result};
use crate::event::{PageDegradeReason, RecoveryKind};
use crate::geometry::{PageFrame, PageGeometryResolveError};
use crate::il::{FontRef, Point, Rect, TextTransform};
use crate::pdf_stream;
use font::ResolvedFont;
use tokenizer::{CompositeDelimiter, InlineImageLengthSource, Token, TokenKind, tokenize};

pub(crate) const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_FORM_DEPTH: usize = 64;
/// Form `/BBox` 裁剪判定的边界容差。字符只有超出裁剪框这么多才算被裁掉，
/// 让浮点噪声与刚好贴边的字形留在可见集里。
const FORM_CLIP_TOLERANCE_PT: f64 = 0.01;

#[derive(Debug, Clone, PartialEq)]
pub struct WalkedChar {
    pub unicode: Option<char>,
    pub unicode_provenance: UnicodeProvenance,
    pub code: u32,
    pub visible: bool,
    pub locatable: bool,
    pub encoded: Vec<u8>,
    pub font: FontRef,
    pub is_bold: bool,
    pub font_size: f64,
    pub advance: f64,
    pub font_supported: bool,
    pub engine_mismatch_tolerated: bool,
    pub baseline_origin: Point,
    pub metric_box: Rect,
    pub text_transform: TextTransform,
    pub content_transform: [f64; 6],
    pub text_matrix_before_glyph: [f64; 6],
    pub source_glyph_scalar_count: usize,
    pub text_line_matrix: [f64; 6],
    pub text_matrix_after_show: [f64; 6],
    pub horizontal_scale: f64,
    pub content_object: ObjectId,
    pub byte_start: usize,
    pub byte_end: usize,
}

/// A byte-exact, self-contained horizontal stroke program whose graphics-state
/// scope can be replayed as one unit. Ownership is deliberately decided later,
/// against formula geometry; the walker only records safe candidates.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WalkedVectorPath {
    pub content_object: ObjectId,
    pub byte_start: usize,
    pub byte_end: usize,
    pub start: Point,
    pub end: Point,
    pub content_transform: [f64; 6],
    pub safe_to_replay: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WalkedInlineImage {
    pub content_object: ObjectId,
    pub byte_start: usize,
    pub byte_end: usize,
    pub bounds: Rect,
    pub content_transform: [f64; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnicodeProvenance {
    ToUnicode,
    EmbeddedFontCmap,
    EmbeddedType1Encoding,
    SimpleEncoding,
    DifferencesAgl,
    Unresolved,
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

    fn is_singular(self) -> bool {
        let [a, b, c, d, _, _] = self.0;
        (a * d - b * c).abs() <= 1e-12
    }

    fn page_rotation(degrees: i32) -> Self {
        match degrees.rem_euclid(360) {
            0 => Self::IDENTITY,
            90 => Self([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
            180 => Self([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
            270 => Self([0.0, -1.0, 1.0, 0.0, 0.0, 0.0]),
            _ => unreachable!("page rotation is validated before walking"),
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
    line_width: f64,
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
            line_width: 1.0,
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
    vector_paths: Vec<WalkedVectorPath>,
    inline_images: Vec<WalkedInlineImage>,
    content_object: ObjectId,
    recoveries: BTreeSet<RecoveryKind>,
    graphics_stack: Vec<GraphicsState>,
    compatibility_depth: usize,
    text_object_is_implicit: bool,
    active_forms: Vec<ObjectId>,
    form_cycles: Vec<Vec<ObjectId>>,
    normalized_form_object_ids: BTreeSet<ObjectId>,
    /// 累积的 Form `/BBox` 裁剪框（页面坐标，轴对齐）。`None` = 页面本体，无 form 裁剪。
    form_clip: Option<Rect>,
    clipped_form_object_ids: BTreeSet<ObjectId>,
    degradation: Option<PageDegradeReason>,
    visual_rotation: Matrix,
    current_operator: Option<(ObjectId, Range<usize>)>,
    vector_scopes: Vec<VectorScope>,
}

#[derive(Debug)]
struct VectorScope {
    content_object: ObjectId,
    byte_start: usize,
    content_transform: [f64; 6],
    points: Vec<Point>,
    stroke_count: usize,
    safe: bool,
}

struct ScopeSnapshot {
    resources: Dictionary,
    state: GraphicsState,
    operands: Vec<Token>,
    graphics_stack: Vec<GraphicsState>,
    compatibility_depth: usize,
    text_object_is_implicit: bool,
    content_object: ObjectId,
    form_clip: Option<Rect>,
}

/// 一页走查的结果。`recoveries` 用集合而非计数：ADR-0013 §3 要求恢复决定
/// **每页一致**，所以消费者要知道的是「这一页用过哪几类恢复」，
/// 而不是「用过多少次」——后者会随内容长度漂移，做不成稳定断言。
#[derive(Debug, Clone, PartialEq)]
pub struct PageWalk {
    pub characters: Vec<WalkedChar>,
    pub(crate) vector_paths: Vec<WalkedVectorPath>,
    pub(crate) inline_images: Vec<WalkedInlineImage>,
    pub recoveries: BTreeSet<RecoveryKind>,
    pub form_cycles: Vec<Vec<ObjectId>>,
    pub normalized_form_object_ids: BTreeSet<ObjectId>,
    pub clipped_form_object_ids: BTreeSet<ObjectId>,
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
    let rotate_degrees = match PageFrame::resolve(document, page_id) {
        Ok(frame) => frame.rotate_degrees,
        Err(PageGeometryResolveError::Degraded { reason, source }) => {
            return Err(PageWalkError::Degraded { reason, source });
        }
        Err(PageGeometryResolveError::Fatal(error)) => return Err(PageWalkError::Fatal(error)),
    };
    walk_page_detailed_with_rotation(document, page_id, rotate_degrees)
}

pub(crate) fn walk_page_detailed_with_rotation(
    document: &Document,
    page_id: ObjectId,
    rotate_degrees: i32,
) -> std::result::Result<PageWalk, PageWalkError> {
    let resources = inherited_page_resources(document, page_id).map_err(PageWalkError::Fatal)?;
    let content_objects = document.get_page_contents(page_id);
    let mut walker = Walker {
        document,
        resources,
        state: GraphicsState::default(),
        operands: Vec::new(),
        characters: Vec::new(),
        vector_paths: Vec::new(),
        inline_images: Vec::new(),
        content_object: (0, 0),
        recoveries: BTreeSet::new(),
        graphics_stack: Vec::new(),
        compatibility_depth: 0,
        text_object_is_implicit: false,
        active_forms: Vec::new(),
        form_cycles: Vec::new(),
        normalized_form_object_ids: BTreeSet::new(),
        form_clip: None,
        clipped_form_object_ids: BTreeSet::new(),
        degradation: None,
        visual_rotation: Matrix::page_rotation(rotate_degrees),
        current_operator: None,
        vector_scopes: Vec::new(),
    };
    let mut content_streams = Vec::with_capacity(content_objects.len());
    for object_id in content_objects {
        let content = document.get_object(object_id).map_err(|error| {
            PageWalkError::Fatal(MimusError::input(
                InputReason::PdfParse,
                format!("page content {} is missing: {error}", object_id.0),
            ))
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
        let decoded = pdf_stream::decode(document, stream, MAX_STREAM_BYTES).map_err(|error| {
            PageWalkError::Degraded {
                reason: PageDegradeReason::ContentDecode,
                source: walk_error(format!("could not decode content {}: {error}", object_id.0)),
            }
        })?;
        walker.content_object = object_id;
        let tokens = tokenize(&decoded).map_err(|failure| PageWalkError::Degraded {
            reason: failure.reason,
            source: failure.into_mimus_error(),
        })?;
        if let Err(error) = walker.walk(tokens) {
            return Err(walker.classify_error(error));
        }
        content_streams.push(WalkedContentStream { object_id, decoded });
    }
    walker.finish();
    Ok(PageWalk {
        characters: walker.characters,
        vector_paths: walker.vector_paths,
        inline_images: walker.inline_images,
        recoveries: walker.recoveries,
        form_cycles: walker.form_cycles,
        normalized_form_object_ids: walker.normalized_form_object_ids,
        clipped_form_object_ids: walker.clipped_form_object_ids,
        content_streams,
    })
}

impl Walker<'_> {
    fn walk(&mut self, tokens: Vec<Token>) -> Result<()> {
        for mut token in tokens {
            token.content_object = self.content_object;
            match &token.kind {
                TokenKind::InlineImage { length_source, .. } => {
                    if let Some(scope) = self.vector_scopes.last_mut() {
                        scope.safe = false;
                    }
                    let corners = [
                        self.state.ctm.point(0.0, 0.0),
                        self.state.ctm.point(1.0, 0.0),
                        self.state.ctm.point(0.0, 1.0),
                        self.state.ctm.point(1.0, 1.0),
                    ];
                    self.inline_images.push(WalkedInlineImage {
                        content_object: token.content_object,
                        byte_start: token.span.start,
                        byte_end: token.span.end,
                        bounds: Rect {
                            left: corners
                                .iter()
                                .map(|point| point.x)
                                .fold(f64::INFINITY, f64::min),
                            bottom: corners
                                .iter()
                                .map(|point| point.y)
                                .fold(f64::INFINITY, f64::min),
                            right: corners
                                .iter()
                                .map(|point| point.x)
                                .fold(f64::NEG_INFINITY, f64::max),
                            top: corners
                                .iter()
                                .map(|point| point.y)
                                .fold(f64::NEG_INFINITY, f64::max),
                        },
                        content_transform: self.state.ctm.0,
                    });
                    if !self.operands.is_empty() {
                        self.recoveries.insert(RecoveryKind::ArityExcess);
                        self.operands.clear();
                    }
                    if *length_source == InlineImageLengthSource::EiScan {
                        self.recoveries.insert(RecoveryKind::InlineImageEiScan);
                    }
                }
                TokenKind::Operator(operator) => {
                    self.current_operator = Some((token.content_object, token.span.clone()));
                    if let Some((first, second)) = split_double_decimal(operator) {
                        self.operands.push(number_token(
                            first,
                            token.span.clone(),
                            token.content_object,
                        ));
                        self.operands.push(number_token(
                            second,
                            token.span.clone(),
                            token.content_object,
                        ));
                        self.recoveries.insert(RecoveryKind::DoubleDecimal);
                        self.current_operator = None;
                        continue;
                    }
                    if let Some((number, recovered_operator)) = split_glued_operator(operator) {
                        self.operands.push(number_token(
                            number,
                            token.span.clone(),
                            token.content_object,
                        ));
                        let operands = std::mem::take(&mut self.operands);
                        self.apply_operator(recovered_operator, &operands)?;
                        self.recoveries.insert(RecoveryKind::GluedToken);
                        self.current_operator = None;
                        continue;
                    }
                    let operands = std::mem::take(&mut self.operands);
                    self.apply_operator(operator, &operands)?;
                    self.current_operator = None;
                }
                _ => self.operands.push(token),
            }
        }
        Ok(())
    }

    fn apply_operator(&mut self, operator: &[u8], operands: &[Token]) -> Result<()> {
        if let Some(scope) = self.vector_scopes.last_mut()
            && !matches!(
                operator,
                b"q" | b"Q"
                    | b"cm"
                    | b"w"
                    | b"J"
                    | b"j"
                    | b"M"
                    | b"d"
                    | b"ri"
                    | b"i"
                    | b"m"
                    | b"l"
                    | b"S"
            )
        {
            scope.safe = false;
        }
        match operator {
            b"BX" => self.compatibility_depth = self.compatibility_depth.saturating_add(1),
            b"EX" => {
                if self.compatibility_depth == 0 {
                    self.recoveries.insert(RecoveryKind::CompatibilityUnderflow);
                } else {
                    self.compatibility_depth -= 1;
                }
            }
            b"q" => {
                if let Some(parent) = self.vector_scopes.last_mut() {
                    parent.safe = false;
                }
                self.graphics_stack.push(self.state.clone());
                if let Some((content_object, span)) = &self.current_operator {
                    self.vector_scopes.push(VectorScope {
                        content_object: *content_object,
                        byte_start: span.start,
                        content_transform: self.state.ctm.0,
                        points: Vec::new(),
                        stroke_count: 0,
                        safe: true,
                    });
                }
            }
            b"Q" => {
                self.finish_vector_scope();
                self.restore_graphics_state();
            }
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
            b"w" => self.set_graphics_number(operands, |state, value| {
                state.line_width = value;
            }),
            b"m" => self.record_vector_point(operands, true),
            b"l" => self.record_vector_point(operands, false),
            b"S" => {
                if let Some(scope) = self.vector_scopes.last_mut() {
                    scope.stroke_count += 1;
                }
            }
            b"Tj" => {
                self.enter_text_phase();
                if let Some(tail) = self.operand_tail(operands, 1) {
                    if let TokenKind::Bytes(bytes) = &tail[0].kind {
                        let first_character = self.characters.len();
                        let line_matrix = self.state.line_matrix;
                        self.show_text(
                            bytes,
                            tail[0].content_object,
                            tail[0].span.start,
                            tail[0].span.end,
                        )?;
                        self.finish_text_show(first_character, line_matrix);
                    } else {
                        self.recoveries.insert(RecoveryKind::InvalidOperands);
                    }
                }
            }
            b"TJ" => {
                self.enter_text_phase();
                let first_character = self.characters.len();
                let line_matrix = self.state.line_matrix;
                self.show_text_array(operands)?;
                self.finish_text_show(first_character, line_matrix);
            }
            b"'" => {
                self.apply_operator(b"T*", &[])?;
                self.apply_operator(b"Tj", operands)?;
            }
            b"\"" => self.show_text_with_spacing(operands)?,
            b"Do" => self.execute_xobject(operands)?,
            _ if is_known_operator(operator) => {}
            _ if self.compatibility_depth > 0 => {}
            _ => {
                self.recoveries.insert(RecoveryKind::UnknownOperator);
            }
        }
        Ok(())
    }

    fn execute_xobject(&mut self, operands: &[Token]) -> Result<()> {
        let Some(tail) = self.operand_tail(operands, 1) else {
            return Ok(());
        };
        let TokenKind::Name(name) = &tail[0].kind else {
            self.recoveries.insert(RecoveryKind::InvalidOperands);
            return Ok(());
        };
        let xobjects = match self
            .resources
            .get_deref(b"XObject", self.document)
            .and_then(Object::as_dict)
        {
            Ok(value) => value.clone(),
            Err(error) => {
                return Err(self.degrade_error(
                    PageDegradeReason::MissingResource,
                    format!(
                        "XObject /{} has no usable resource dictionary: {error}",
                        display_pdf_name(name)
                    ),
                ));
            }
        };
        let object_id = match xobjects.get(name) {
            Ok(value) => match value.as_reference() {
                Ok(object_id) => object_id,
                Err(_) => {
                    return Err(self.degrade_error(
                        PageDegradeReason::XObjectNotAStream,
                        format!(
                            "XObject /{} is not an indirect stream",
                            display_pdf_name(name)
                        ),
                    ));
                }
            },
            Err(error) => {
                return Err(self.degrade_error(
                    PageDegradeReason::MissingResource,
                    format!("XObject /{} is missing: {error}", display_pdf_name(name)),
                ));
            }
        };
        let stream = match self.document.get_object(object_id) {
            Ok(value) => match value.as_stream() {
                Ok(stream) => stream.clone(),
                Err(error) => {
                    return Err(self.degrade_error(
                        PageDegradeReason::XObjectNotAStream,
                        format!(
                            "XObject /{} object {} is not a stream: {error}",
                            display_pdf_name(name),
                            object_id.0
                        ),
                    ));
                }
            },
            Err(error) => {
                return Err(self.degrade_error(
                    PageDegradeReason::MissingResource,
                    format!(
                        "XObject /{} object {} is missing: {error}",
                        display_pdf_name(name),
                        object_id.0
                    ),
                ));
            }
        };
        let subtype = stream.dict.get(b"Subtype").and_then(Object::as_name).ok();
        match subtype {
            Some(b"Image") => Ok(()),
            Some(b"Form") => self.execute_form(name, object_id, &stream),
            _ => Err(self.degrade_error(
                PageDegradeReason::MissingResource,
                format!(
                    "XObject /{} object {} has no supported Subtype",
                    display_pdf_name(name),
                    object_id.0
                ),
            )),
        }
    }

    fn execute_form(
        &mut self,
        name: &[u8],
        object_id: ObjectId,
        stream: &lopdf::Stream,
    ) -> Result<()> {
        if self.active_forms.contains(&object_id) {
            let recovery = if self.active_forms.last() == Some(&object_id) {
                RecoveryKind::SelfRecursiveForm
            } else {
                RecoveryKind::MutuallyRecursiveForm
            };
            self.recoveries.insert(recovery);
            let mut path = self.active_forms.clone();
            path.push(object_id);
            if !self.form_cycles.contains(&path) {
                self.form_cycles.push(path);
            }
            return Ok(());
        }
        if self.active_forms.len() >= MAX_FORM_DEPTH {
            self.recoveries.insert(RecoveryKind::FormDepthExceeded);
            return Ok(());
        }

        let normalized_bbox = numeric_array(self.document, &stream.dict, b"BBox", 4)
            .ok()
            .flatten()
            .and_then(|values| normalize_form_bbox(&values));
        let Some((bbox, reordered)) = normalized_bbox else {
            return Err(self.degrade_error(
                PageDegradeReason::BadFormBBox,
                format!(
                    "Form XObject /{} object {} has no usable BBox",
                    display_pdf_name(name),
                    object_id.0
                ),
            ));
        };
        if reordered {
            self.recoveries.insert(RecoveryKind::NormalizedFormBBox);
            self.normalized_form_object_ids.insert(object_id);
        }
        let matrix = match numeric_array(self.document, &stream.dict, b"Matrix", 6) {
            Ok(Some(values)) if values.iter().all(|value| value.is_finite()) => {
                Matrix::from_values(&values)
            }
            Ok(None) => Matrix::IDENTITY,
            Ok(Some(_)) | Err(_) => {
                return Err(self.degrade_error(
                    PageDegradeReason::BadFormMatrix,
                    format!(
                        "Form XObject /{} object {} has no usable Matrix",
                        display_pdf_name(name),
                        object_id.0
                    ),
                ));
            }
        };
        let resources = match stream.dict.get(b"Resources") {
            Ok(value) => match self
                .document
                .dereference(value)
                .and_then(|(_, value)| value.as_dict())
            {
                Ok(resources) => resources.clone(),
                Err(error) => {
                    return Err(self.degrade_error(
                        PageDegradeReason::MissingResource,
                        format!(
                            "Form XObject /{} object {} has invalid Resources: {error}",
                            display_pdf_name(name),
                            object_id.0
                        ),
                    ));
                }
            },
            Err(_) => self.resources.clone(),
        };
        let decoded = match pdf_stream::decode(self.document, stream, MAX_STREAM_BYTES) {
            Ok(decoded) => decoded,
            Err(error) => {
                return Err(self.degrade_error(
                    PageDegradeReason::ContentDecode,
                    format!(
                        "could not decode Form XObject /{} object {}: {error}",
                        display_pdf_name(name),
                        object_id.0
                    ),
                ));
            }
        };

        // PDF 32000-1:2008 §8.10.2 Table 95：`/BBox` 在 form 坐标系下表述，并且在
        // `/Matrix` 与 CTM 连接之后作为裁剪路径生效。旋转 / 斜切的 form 会把矩形
        // 映射成平行四边形，这里取其轴对齐外接框——**故意取超集**，宁可少裁也不
        // 误裁真实墨迹。
        let clip = transformed_box(
            self.state.ctm.then(matrix),
            bbox[0],
            bbox[1],
            bbox[2],
            bbox[3],
        );
        let form_clip = if rect_is_finite(clip) {
            Some(match self.form_clip {
                Some(inherited) => intersect_rect(inherited, clip),
                None => clip,
            })
        } else {
            self.form_clip
        };

        self.active_forms.push(object_id);
        let snapshot = self.enter_scope(resources, matrix, object_id, form_clip);
        let result = match tokenize(&decoded) {
            Ok(tokens) => self.walk(tokens).map(|()| self.finish_scoped()),
            Err(failure) => {
                self.degradation.get_or_insert(failure.reason);
                Err(failure.into_mimus_error())
            }
        };
        self.restore_scope(snapshot);
        let popped = self.active_forms.pop();
        debug_assert_eq!(popped, Some(object_id));
        result
    }

    fn enter_scope(
        &mut self,
        resources: Dictionary,
        matrix: Matrix,
        content_object: ObjectId,
        form_clip: Option<Rect>,
    ) -> ScopeSnapshot {
        let mut child_state = self.state.clone();
        child_state.ctm = child_state.ctm.then(matrix);
        child_state.text_matrix = Matrix::IDENTITY;
        child_state.line_matrix = Matrix::IDENTITY;
        child_state.phase = TextPhase::Outside;
        ScopeSnapshot {
            resources: std::mem::replace(&mut self.resources, resources),
            state: std::mem::replace(&mut self.state, child_state),
            operands: std::mem::take(&mut self.operands),
            graphics_stack: std::mem::take(&mut self.graphics_stack),
            compatibility_depth: std::mem::replace(&mut self.compatibility_depth, 0),
            text_object_is_implicit: std::mem::replace(&mut self.text_object_is_implicit, false),
            content_object: std::mem::replace(&mut self.content_object, content_object),
            form_clip: std::mem::replace(&mut self.form_clip, form_clip),
        }
    }

    fn restore_scope(&mut self, snapshot: ScopeSnapshot) {
        self.resources = snapshot.resources;
        self.state = snapshot.state;
        self.operands = snapshot.operands;
        self.graphics_stack = snapshot.graphics_stack;
        self.compatibility_depth = snapshot.compatibility_depth;
        self.text_object_is_implicit = snapshot.text_object_is_implicit;
        self.content_object = snapshot.content_object;
        self.form_clip = snapshot.form_clip;
    }

    /// 字符是否整体落在累积 Form 裁剪框之外。只有**完全**在某一侧才判为裁掉，
    /// 部分相交一律保留（ADR-0013 §3：有界、可报告、不扩散）。
    fn clipped_by_form_bbox(&self, metric_box: Rect) -> bool {
        let Some(clip) = self.form_clip else {
            return false;
        };
        if !rect_is_finite(metric_box) {
            return false;
        }
        metric_box.right < clip.left - FORM_CLIP_TOLERANCE_PT
            || metric_box.left > clip.right + FORM_CLIP_TOLERANCE_PT
            || metric_box.top < clip.bottom - FORM_CLIP_TOLERANCE_PT
            || metric_box.bottom > clip.top + FORM_CLIP_TOLERANCE_PT
    }

    fn finish_scoped(&mut self) {
        if !self.operands.is_empty() {
            self.recoveries.insert(RecoveryKind::ScopedDanglingOperands);
            self.operands.clear();
        }
        if !self.graphics_stack.is_empty() {
            self.recoveries
                .insert(RecoveryKind::ScopedGraphicsStateUnclosed);
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

    fn classify_error(&mut self, error: MimusError) -> PageWalkError {
        if let Some(reason) = self.degradation.take() {
            PageWalkError::Degraded {
                reason,
                source: error,
            }
        } else if error.reason() == ErrorReason::Input(InputReason::OperatorWalk) {
            PageWalkError::Degraded {
                reason: PageDegradeReason::ContentStreamSyntax,
                source: error,
            }
        } else {
            PageWalkError::Fatal(error)
        }
    }

    fn degrade_error(
        &mut self,
        reason: PageDegradeReason,
        message: impl Into<String>,
    ) -> MimusError {
        self.degradation.get_or_insert(reason);
        walk_error(message)
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
        self.vector_scopes.clear();
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

    fn set_graphics_number(
        &mut self,
        operands: &[Token],
        update: impl FnOnce(&mut GraphicsState, f64),
    ) {
        if let Some(values) = self.numeric_tail(operands, 1) {
            update(&mut self.state, values[0]);
        }
    }

    fn record_vector_point(&mut self, operands: &[Token], starts_path: bool) {
        let Some(values) = self.numeric_tail(operands, 2) else {
            if let Some(scope) = self.vector_scopes.last_mut() {
                scope.safe = false;
            }
            return;
        };
        let point = self.state.ctm.point(values[0], values[1]);
        let Some(scope) = self.vector_scopes.last_mut() else {
            return;
        };
        if starts_path {
            if !scope.points.is_empty() {
                scope.safe = false;
            }
            scope.points.clear();
        }
        scope.points.push(point);
    }

    fn finish_vector_scope(&mut self) {
        let Some(scope) = self.vector_scopes.pop() else {
            return;
        };
        let Some((content_object, operator_span)) = &self.current_operator else {
            return;
        };
        if scope.stroke_count != 1
            || scope.points.len() != 2
            || scope.content_object != *content_object
        {
            return;
        }
        let start = scope.points[0];
        let end = scope.points[1];
        if (start.y - end.y).abs() > 0.01 || (start.x - end.x).abs() <= 0.01 {
            return;
        }
        self.vector_paths.push(WalkedVectorPath {
            content_object: scope.content_object,
            byte_start: scope.byte_start,
            byte_end: operator_span.end,
            start,
            end,
            content_transform: scope.content_transform,
            safe_to_replay: scope.safe,
        });
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
        self.show_text(
            bytes,
            tail[2].content_object,
            tail[2].span.start,
            tail[2].span.end,
        )
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

    fn show_text(
        &mut self,
        bytes: &[u8],
        content_object: ObjectId,
        byte_start: usize,
        byte_end: usize,
    ) -> Result<()> {
        if self.state.font_name.is_empty() {
            return Err(walk_error("Tj appeared before Tf"));
        }
        let font = ResolvedFont::resolve(self.document, &self.resources, &self.state.font_name)
            .map_err(|object_id| {
                MimusError::input(
                    InputReason::PdfParse,
                    format!(
                        "font /{} points to missing object {} {} R",
                        display_pdf_name(&self.state.font_name),
                        object_id.0,
                        object_id.1
                    ),
                )
            })?;
        if font.normalized_descriptor_descent {
            self.recoveries.insert(RecoveryKind::NormalizedFontDescent);
        }
        for glyph in font.decode(bytes) {
            let glyph_width = glyph.advance_em * self.state.font_size;
            let word_spacing = if glyph.encoded.as_slice() == b" " {
                self.state.word_spacing
            } else {
                0.0
            };
            let total_advance = (glyph_width + self.state.character_spacing + word_spacing)
                * self.state.horizontal_scale;
            let unicode = if glyph.unicode.is_empty() {
                vec![None]
            } else {
                glyph.unicode.into_iter().map(Some).collect::<Vec<_>>()
            };
            let source_glyph_scalar_count = unicode.len();
            let parts = unicode.len() as f64;
            let part_width = glyph_width / parts;
            let part_advance = total_advance / parts;
            for unicode in unicode {
                let text_matrix_before_glyph = self.state.text_matrix.0;
                let transform = self.state.ctm.then(self.state.text_matrix);
                let locatable = !transform.is_singular();
                let baseline = transform.point(0.0, self.state.rise);
                let metric_box = transformed_box(
                    transform,
                    0.0,
                    font.descent_em * self.state.font_size + self.state.rise,
                    part_width * self.state.horizontal_scale,
                    font.ascent_em * self.state.font_size + self.state.rise,
                );
                let clipped_out = self.clipped_by_form_bbox(metric_box);
                if clipped_out {
                    self.recoveries.insert(RecoveryKind::ClippedFormContent);
                    if let Some(&form) = self.active_forms.last() {
                        self.clipped_form_object_ids.insert(form);
                    }
                }
                self.characters.push(WalkedChar {
                    unicode,
                    unicode_provenance: glyph.unicode_provenance,
                    code: glyph.code,
                    visible: !matches!(self.state.rendering_mode, 3 | 7) && !clipped_out,
                    locatable,
                    encoded: glyph.encoded.clone(),
                    font: font.reference.clone(),
                    is_bold: font.is_bold,
                    font_size: self.state.font_size,
                    advance: part_width * self.state.horizontal_scale,
                    font_supported: glyph.font_supported,
                    engine_mismatch_tolerated: font.engine_mismatch_tolerated,
                    baseline_origin: baseline,
                    metric_box,
                    text_transform: classify_transform(self.visual_rotation.then(transform)),
                    content_transform: self.state.ctm.0,
                    text_matrix_before_glyph,
                    source_glyph_scalar_count,
                    text_line_matrix: self.state.line_matrix.0,
                    text_matrix_after_show: self.state.text_matrix.0,
                    horizontal_scale: self.state.horizontal_scale,
                    content_object,
                    byte_start,
                    byte_end,
                });
                self.state.text_matrix = self.state.text_matrix.translate(part_advance, 0.0);
            }
        }
        Ok(())
    }

    fn finish_text_show(&mut self, first_character: usize, line_matrix: Matrix) {
        let text_matrix_after_show = self.state.text_matrix.0;
        for character in &mut self.characters[first_character..] {
            character.text_line_matrix = line_matrix.0;
            character.text_matrix_after_show = text_matrix_after_show;
        }
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
        let array_start = operands[start].span.start;
        let array_end = last.span.end;
        let content_object = operands[start].content_object;
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
                    self.show_text(bytes, content_object, array_start, array_end)?;
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

fn number_token(value: f64, span: std::ops::Range<usize>, content_object: ObjectId) -> Token {
    Token {
        kind: TokenKind::Number(value),
        span,
        content_object,
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

fn rect_is_finite(rect: Rect) -> bool {
    [rect.left, rect.bottom, rect.right, rect.top]
        .iter()
        .all(|value| value.is_finite())
}

fn intersect_rect(left: Rect, right: Rect) -> Rect {
    Rect {
        left: left.left.max(right.left),
        bottom: left.bottom.max(right.bottom),
        right: left.right.min(right.right),
        top: left.top.min(right.top),
    }
}

fn normalize_form_bbox(values: &[f64]) -> Option<([f64; 4], bool)> {
    let [x0, y0, x1, y1]: [f64; 4] = values.try_into().ok()?;
    if !values.iter().all(|value| value.is_finite()) || x0 == x1 || y0 == y1 {
        return None;
    }
    Some((
        [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)],
        x0 > x1 || y0 > y1,
    ))
}

fn numeric_array(
    document: &Document,
    dictionary: &Dictionary,
    key: &[u8],
    arity: usize,
) -> Result<Option<Vec<f64>>> {
    if !dictionary.has(key) {
        return Ok(None);
    }
    let values = dictionary
        .get_deref(key, document)
        .and_then(Object::as_array)
        .map_err(|error| {
            walk_error(format!(
                "/{} must be an array: {error}",
                display_pdf_name(key)
            ))
        })?;
    if values.len() != arity {
        return Err(walk_error(format!(
            "/{} must contain exactly {arity} values, found {}",
            display_pdf_name(key),
            values.len()
        )));
    }
    values
        .iter()
        .map(|value| {
            let (_, value) = document.dereference(value).map_err(|error| {
                walk_error(format!(
                    "/{} contains an invalid reference: {error}",
                    display_pdf_name(key)
                ))
            })?;
            object_number(value).ok_or_else(|| {
                walk_error(format!(
                    "/{} contains a non-numeric value",
                    display_pdf_name(key)
                ))
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
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
    // 斜切超过 20deg 才隔离。调用方传入 R(/Rotate) * CTM * Tm 的视觉线性变换。
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

    use lopdf::dictionary;

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

    fn walk_form_chain(depth: usize) -> PageWalk {
        assert!(depth > 0);
        let mut document = Document::load(fixture()).unwrap();
        let mut next = None;
        for _ in 0..depth {
            let mut resources = Dictionary::new();
            resources.set("Font", lopdf::dictionary! { "F1" => (5, 0) });
            let content = if let Some(next) = next {
                resources.set("XObject", lopdf::dictionary! { "Next" => next });
                b"/Next Do\n".to_vec()
            } else {
                b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET\n".to_vec()
            };
            next = Some(document.add_object(lopdf::Stream::new(
                lopdf::dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
                    "Resources" => resources,
                },
                content,
            )));
        }
        document
            .get_object_mut((4, 0))
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "XObject",
                lopdf::dictionary! { "Root" => next.expect("non-empty Form chain") },
            );
        document
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(b"/Root Do\n".to_vec());
        let page_id = document.get_pages()[&1];
        walk_page(&document, page_id).unwrap()
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
        assert!(
            characters
                .iter()
                .all(|character| character.unicode_provenance == UnicodeProvenance::ToUnicode)
        );
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
    fn production_walk_classifies_text_in_the_rotated_visual_page_frame() {
        let mut document = Document::load(fixture()).unwrap();
        let page_id = document.get_pages()[&1];
        document
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Rotate", 90);
        document
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(
                b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (M) Tj 0 -1 1 0 72 120 Tm (I) Tj ET\n".to_vec(),
            );

        let characters = walk_page(&document, page_id).unwrap().characters;

        assert_eq!(characters.len(), 2);
        assert_eq!(characters[0].text_transform, TextTransform::Rotated(90.0));
        assert_eq!(characters[1].text_transform, TextTransform::Upright);
    }

    #[test]
    fn production_walk_handles_the_realistic_rotated_fixture() {
        let walked = walk_fixture("unit-geom-01-rotate-90");

        assert!(!walked.characters.is_empty());
        assert!(
            walked
                .characters
                .iter()
                .all(|character| character.text_transform == TextTransform::Rotated(90.0))
        );
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
    fn production_walk_resolves_an_indirect_content_filter() {
        let mut document = Document::load(fixture()).unwrap();
        let mut program = vec![b'%'];
        program.extend(std::iter::repeat_n(b'A', 512));
        program.push(b'\n');
        program.extend_from_slice(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET");
        let filter = {
            let stream = document
                .get_object_mut((9, 0))
                .unwrap()
                .as_stream_mut()
                .unwrap();
            stream.set_plain_content(program);
            stream.compress().unwrap();
            stream.dict.remove(b"Filter").unwrap()
        };
        let filter_id = document.add_object(filter);
        document
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .dict
            .set("Filter", filter_id);

        let page_id = document.get_pages()[&1];
        let walked = walk_page(&document, page_id).unwrap();
        assert_eq!(text_of(&walked), "MIMUS");
        assert!(walked.recoveries.is_empty());
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
    fn production_walk_keeps_a_cross_stream_text_operand_owned_by_its_source_stream() {
        let document =
            Document::load(fixture_path("unit-parse-12-contents-array-tj-operand")).unwrap();
        let page_id = document.get_pages()[&1];

        let walked = walk_page(&document, page_id).unwrap();

        assert_eq!(text_of(&walked), "MIMUS");
        assert!(walked.characters.iter().all(|character| {
            character.content_object == (9, 0)
                && character.byte_end <= walked.content_streams[0].decoded.len()
        }));
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
    fn production_walk_decodes_type0_type3_and_file_defined_standard14_widths() {
        for (id, expected_text, expected_advance, expected_provenance) in [
            (
                "unit-cmap-01-identity-no-tounicode",
                "MIMUS",
                10.356,
                UnicodeProvenance::EmbeddedFontCmap,
            ),
            (
                "unit-stream-02-type3-d1",
                "M",
                12.0,
                UnicodeProvenance::SimpleEncoding,
            ),
            (
                "unit-stream-04-type3-d0",
                "M",
                12.0,
                UnicodeProvenance::SimpleEncoding,
            ),
            (
                "unit-font-01-std14-custom-widths",
                "AAAA",
                12.0,
                UnicodeProvenance::SimpleEncoding,
            ),
        ] {
            let walked = walk_fixture(id);
            assert_eq!(text_of(&walked), expected_text, "fixture {id}");
            assert!(
                walked.characters.iter().all(|character| {
                    character.font_supported
                        && character.advance.is_finite()
                        && character.advance > 0.0
                }),
                "fixture {id}"
            );
            assert!(
                walked
                    .characters
                    .iter()
                    .all(|character| { character.unicode_provenance == expected_provenance }),
                "fixture {id}"
            );
            assert!(
                (walked.characters[0].advance - expected_advance).abs() < 0.001,
                "fixture {id}"
            );
        }
    }

    #[test]
    fn simple_font_fallback_requires_the_character_in_the_embedded_cmap() {
        let mut document = Document::load(fixture_path("unit-font-escaped-name")).unwrap();
        document
            .get_object_mut((8, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(b"BT /F#31 12 Tf 1 0 0 1 72 120 Tm (Z) Tj ET".to_vec());

        let page_id = document.get_pages()[&1];
        let walked = walk_page(&document, page_id).unwrap();

        assert_eq!(walked.characters.len(), 1);
        assert_eq!(walked.characters[0].code, u32::from(b'Z'));
        assert_eq!(walked.characters[0].unicode, None);
        assert_eq!(
            walked.characters[0].unicode_provenance,
            UnicodeProvenance::Unresolved
        );
        assert!(walked.characters[0].font_supported);
        assert!(walked.characters[0].advance > 0.0);
    }

    #[test]
    fn a_used_dangling_font_reference_is_a_fatal_parse_error() {
        let mut document = Document::load(fixture()).unwrap();
        document
            .get_object_mut((4, 0))
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .get_mut(b"Font")
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("F1", (99, 0));
        let page_id = document.get_pages()[&1];

        assert!(matches!(
            walk_page_detailed(&document, page_id),
            Err(PageWalkError::Fatal(error))
                if error.reason() == ErrorReason::Input(InputReason::PdfParse)
        ));
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
        let program = b"BT /F1 12 Tf 1 0 0 1 72 120 Tm [(MIMUS)] TJ ET";
        let walked = walk_program(program).unwrap();
        assert_eq!(text_of(&walked), "MIMUS");
        assert_eq!(
            walked.characters[0].baseline_origin,
            Point { x: 72.0, y: 120.0 }
        );
        let array_start = program.iter().position(|byte| *byte == b'[').unwrap();
        let array_end = program.iter().position(|byte| *byte == b']').unwrap() + 1;
        assert!(walked.characters.iter().all(|character| {
            character.byte_start == array_start && character.byte_end == array_end
        }));
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
    fn nested_bt_resets_text_matrices_and_reports_recovery() {
        let walked = walk_program(
            b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MI) Tj BT /F1 12 Tf 1 0 0 1 100 80 Tm (MUS) Tj ET",
        )
        .unwrap();

        assert_eq!(text_of(&walked), "MIMUS");
        assert_eq!(
            walked.characters[2].baseline_origin,
            Point { x: 100.0, y: 80.0 }
        );
        assert_eq!(
            walked.recoveries,
            BTreeSet::from([RecoveryKind::NestedTextObject])
        );
    }

    #[test]
    fn production_walk_pads_odd_hex_and_degrades_invalid_hex() {
        let odd = walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm <4D494D55534> Tj ET").unwrap();
        assert_eq!(
            odd.characters
                .iter()
                .map(|character| character.code)
                .collect::<Vec<_>>(),
            vec![0x4d, 0x49, 0x4d, 0x55, 0x53, 0x40]
        );
        assert_eq!(odd.characters.last().unwrap().encoded, vec![0x40]);
        assert!(odd.recoveries.is_empty());

        let mut document = Document::load(fixture()).unwrap();
        document
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm <4G> Tj ET".to_vec());
        let page_id = document.get_pages()[&1];
        assert!(matches!(
            walk_page_detailed(&document, page_id),
            Err(PageWalkError::Degraded {
                reason: PageDegradeReason::ContentStreamSyntax,
                ..
            })
        ));
    }

    #[test]
    fn tr_seven_characters_are_invisible_without_hiding_later_text() {
        let walked =
            walk_program(b"BT /F1 12 Tf 1 0 0 1 72 120 Tm 7 Tr (MI) Tj 0 Tr (MUS) Tj ET").unwrap();

        assert_eq!(text_of(&walked), "MIMUS");
        assert!(
            walked.characters[..2]
                .iter()
                .all(|character| !character.visible)
        );
        assert!(
            walked.characters[2..]
                .iter()
                .all(|character| character.visible)
        );
        assert!(walked.recoveries.is_empty());
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
                "MIMUS",
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
    fn production_walk_executes_nested_forms_with_inherited_resources_and_ctms() {
        let mut document =
            Document::load(fixture_path("unit-xobj-04-inherited-resources")).unwrap();
        // The fixture intentionally uses Standard-14 fonts without explicit metrics, which remain
        // outside M1. Rebind only the two font objects to its pinned explicit-metrics font so this
        // test isolates Form resource inheritance and matrix composition.
        let explicit_font = document.get_object((5, 0)).unwrap().clone();
        document.objects.insert((9, 0), explicit_font.clone());
        document.objects.insert((13, 0), explicit_font);

        let page_id = document.get_pages()[&1];
        let walked = walk_page(&document, page_id).unwrap();

        assert_eq!(text_of(&walked), "IIIIII");
        assert_eq!(walked.characters.last().unwrap().code, u32::from(b'H'));
        assert_eq!(walked.characters.last().unwrap().unicode, None);
        assert_eq!(
            walked.characters[0].baseline_origin,
            Point { x: 110.0, y: 176.0 }
        );
        assert_eq!(
            walked.characters[3].baseline_origin,
            Point { x: 72.0, y: 80.0 }
        );
        assert!(walked.recoveries.is_empty());
    }

    #[test]
    fn production_walk_reports_form_cycles_by_indirect_object_path() {
        for (id, recovery, path) in [
            (
                "mal-xobj-01-self-recursive",
                RecoveryKind::SelfRecursiveForm,
                vec![(11, 0), (11, 0)],
            ),
            (
                "mal-xobj-02-mutual-recursive",
                RecoveryKind::MutuallyRecursiveForm,
                vec![(12, 0), (13, 0), (12, 0)],
            ),
        ] {
            let walked = walk_fixture(id);
            assert!(walked.characters.is_empty(), "fixture {id}");
            assert_eq!(
                walked.recoveries,
                BTreeSet::from([recovery]),
                "fixture {id}"
            );
            assert_eq!(walked.form_cycles, vec![path], "fixture {id}");
        }
    }

    #[test]
    fn production_walk_degrades_forms_with_bad_required_geometry() {
        let document = Document::load(fixture_path("mal-xobj-03-form-no-bbox")).unwrap();
        let page_id = document.get_pages()[&1];
        assert!(matches!(
            walk_page_detailed(&document, page_id),
            Err(PageWalkError::Degraded {
                reason: PageDegradeReason::BadFormBBox,
                ..
            })
        ));

        let mut document = Document::load(fixture_path("unit-xobj-00-recursion-parent")).unwrap();
        document
            .get_object_mut((10, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .dict
            .set("Matrix", vec![1.into(), 0.into(), 0.into()]);
        let page_id = document.get_pages()[&1];
        assert!(matches!(
            walk_page_detailed(&document, page_id),
            Err(PageWalkError::Degraded {
                reason: PageDegradeReason::BadFormMatrix,
                ..
            })
        ));
    }

    /// 页面调用 `/Outer`（BBox 上沿 `outer_top`），`/Outer` 再调用 `/Inner`
    /// （BBox 上沿 200），`/Inner` 在 `baseline_y` 处画 MIMUS。用来验证裁剪框
    /// 沿嵌套链求交，而不是只看最内层。
    fn walk_nested_form_clip(outer_top: i32, baseline_y: i32) -> PageWalk {
        let mut document = Document::load(fixture()).unwrap();
        let mut inner_resources = Dictionary::new();
        inner_resources.set("Font", lopdf::dictionary! { "F1" => (5, 0) });
        let inner = document.add_object(lopdf::Stream::new(
            lopdf::dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
                "Resources" => inner_resources,
            },
            format!("BT /F1 12 Tf 1 0 0 1 72 {baseline_y} Tm (MIMUS) Tj ET\n").into_bytes(),
        ));
        let outer = document.add_object(lopdf::Stream::new(
            lopdf::dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 300.into(), outer_top.into()],
                "Resources" => lopdf::dictionary! {
                    "XObject" => lopdf::dictionary! { "Inner" => inner },
                },
            },
            b"/Inner Do\n".to_vec(),
        ));
        document
            .get_object_mut((4, 0))
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("XObject", lopdf::dictionary! { "Outer" => outer });
        document
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(b"/Outer Do\n".to_vec());
        let page_id = document.get_pages()[&1];
        walk_page(&document, page_id).unwrap()
    }

    #[test]
    fn production_walk_marks_form_text_outside_the_bbox_invisible_without_losing_it() {
        let document = Document::load(fixture_path("unit-xobj-12-form-bbox-clip")).unwrap();
        let pages = document.get_pages();

        let inside = walk_page(&document, pages[&1]).unwrap();
        assert_eq!(text_of(&inside), "MIMUSMIMUS");
        assert!(inside.characters.iter().all(|character| character.visible));
        assert!(inside.recoveries.is_empty());
        assert!(inside.clipped_form_object_ids.is_empty());

        let clipped = walk_page(&document, pages[&2]).unwrap();
        // 被裁掉的字符仍然留在走查结果里——提取视图（poppler / PDFium text page）
        // 也看得到它们，抽掉会凭空制造 engine-only 残差（ADR-0015）。
        assert_eq!(text_of(&clipped), "MIMUSMIMUS");
        assert_eq!(
            clipped
                .characters
                .iter()
                .filter(|character| !character.visible)
                .map(|character| (character.unicode, character.baseline_origin.y))
                .collect::<Vec<_>>(),
            [
                (Some('M'), 125.0),
                (Some('I'), 125.0),
                (Some('M'), 125.0),
                (Some('U'), 125.0),
                (Some('S'), 125.0),
            ]
        );
        assert_eq!(
            clipped.recoveries,
            BTreeSet::from([RecoveryKind::ClippedFormContent])
        );
        assert_eq!(
            clipped.clipped_form_object_ids,
            BTreeSet::from([(11, 0)]),
            "the innermost Form owning the clipped ink is reported"
        );
    }

    #[test]
    fn form_bbox_clipping_intersects_across_nesting_and_keeps_straddling_glyphs() {
        // 内层 BBox 上沿 200 容得下 y=125 的文字，外层上沿 100 容不下：裁剪框必须求交。
        let clipped = walk_nested_form_clip(100, 125);
        assert_eq!(text_of(&clipped), "MIMUS");
        assert!(
            clipped
                .characters
                .iter()
                .all(|character| !character.visible)
        );
        assert_eq!(
            clipped.recoveries,
            BTreeSet::from([RecoveryKind::ClippedFormContent])
        );

        // 外层放宽到 200 后同一段文字照常可见——变量只有外层 BBox 上沿。
        let inside = walk_nested_form_clip(200, 125);
        assert!(inside.characters.iter().all(|character| character.visible));
        assert!(inside.recoveries.is_empty());

        // 跨越裁剪边界的字形保守保留：度量盒 [95.168, 109.136] 与上沿 100 相交。
        let straddling = walk_nested_form_clip(100, 98);
        assert!(
            straddling
                .characters
                .iter()
                .all(|character| character.visible)
        );
        assert!(straddling.clipped_form_object_ids.is_empty());
    }

    #[test]
    fn production_walk_accepts_reversed_form_bbox_with_text_outside_or_inside_the_form() {
        for (fixture, recovered_page, unaffected_page, form_object) in [
            ("mal-xobj-11-reversed-bbox-page-text", 1, 2, (10, 0)),
            ("mal-xobj-11-reversed-bbox-form-text", 2, 1, (13, 0)),
        ] {
            let document = Document::load(fixture_path(fixture)).unwrap();
            let pages = document.get_pages();
            let walked = walk_page_detailed(&document, pages[&recovered_page])
                .unwrap_or_else(|error| panic!("fixture {fixture}: {error:?}"));
            assert_eq!(text_of(&walked), "MIMUS", "fixture {fixture}");
            assert_eq!(
                walked.recoveries,
                BTreeSet::from([RecoveryKind::NormalizedFormBBox]),
                "fixture {fixture}"
            );
            assert_eq!(
                walked.normalized_form_object_ids,
                BTreeSet::from([form_object]),
                "fixture {fixture}"
            );

            let unaffected = walk_page_detailed(&document, pages[&unaffected_page])
                .unwrap_or_else(|error| panic!("fixture {fixture} sibling: {error:?}"));
            assert_eq!(text_of(&unaffected), "MIMUS", "fixture {fixture} sibling");
            assert!(
                unaffected.recoveries.is_empty(),
                "fixture {fixture} sibling"
            );
            assert!(
                unaffected.normalized_form_object_ids.is_empty(),
                "fixture {fixture} sibling"
            );
        }
    }

    #[test]
    fn form_bbox_normalization_rejects_nonfinite_wrong_arity_and_degenerate_values() {
        assert_eq!(
            normalize_form_bbox(&[20.0, 180.0, 0.0, 0.0]),
            Some(([0.0, 0.0, 20.0, 180.0], true))
        );
        assert_eq!(
            normalize_form_bbox(&[0.0, 0.0, 20.0, 180.0]),
            Some(([0.0, 0.0, 20.0, 180.0], false))
        );
        for invalid in [
            &[0.0, 0.0, 20.0][..],
            &[0.0, 0.0, f64::INFINITY, 180.0],
            &[0.0, f64::NAN, 20.0, 180.0],
            &[0.0, 0.0, 0.0, 180.0],
            &[0.0, 180.0, 20.0, 180.0],
        ] {
            assert_eq!(normalize_form_bbox(invalid), None, "values {invalid:?}");
        }
    }

    #[test]
    fn production_walk_classifies_missing_and_non_stream_xobjects() {
        let mut missing = Document::load(fixture()).unwrap();
        missing
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(b"/Missing Do".to_vec());
        let page_id = missing.get_pages()[&1];
        assert!(matches!(
            walk_page_detailed(&missing, page_id),
            Err(PageWalkError::Degraded {
                reason: PageDegradeReason::MissingResource,
                ..
            })
        ));

        let mut non_stream = Document::load(fixture()).unwrap();
        let object_id = non_stream.add_object(Object::Integer(42));
        non_stream
            .get_object_mut((4, 0))
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("XObject", dictionary! { "Broken" => object_id });
        non_stream
            .get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(b"/Broken Do".to_vec());
        let page_id = non_stream.get_pages()[&1];
        assert!(matches!(
            walk_page_detailed(&non_stream, page_id),
            Err(PageWalkError::Degraded {
                reason: PageDegradeReason::XObjectNotAStream,
                ..
            })
        ));
    }

    #[test]
    fn form_scopes_cannot_pop_or_leak_caller_state() {
        let underflow = walk_fixture("mal-xobj-04-scope-underflow");
        assert_eq!(text_of(&underflow), "MIMUS");
        assert_eq!(
            underflow.characters[0].baseline_origin,
            Point { x: 72.0, y: 120.0 }
        );
        assert_eq!(
            underflow.recoveries,
            BTreeSet::from([RecoveryKind::GraphicsStateUnderflow])
        );

        let tail = walk_fixture("mal-xobj-05-scope-tail");
        assert_eq!(text_of(&tail), "MIMUS");
        assert_eq!(
            tail.characters[0].baseline_origin,
            Point { x: 72.0, y: 120.0 }
        );
        assert_eq!(
            tail.recoveries,
            BTreeSet::from([
                RecoveryKind::ScopedDanglingOperands,
                RecoveryKind::ScopedGraphicsStateUnclosed,
            ])
        );
    }

    #[test]
    fn singular_form_characters_are_unlocatable_without_poisoning_the_page() {
        let walked = walk_fixture("unit-xobj-05-singular-ctm");
        assert_eq!(text_of(&walked), "MMIMUS");
        assert_eq!(
            walked.characters[..4]
                .iter()
                .map(|character| character.code)
                .collect::<Vec<_>>(),
            b"FORM".iter().copied().map(u32::from).collect::<Vec<_>>()
        );
        assert!(
            walked.characters[..4]
                .iter()
                .all(|character| !character.locatable)
        );
        assert!(
            walked.characters[4..]
                .iter()
                .all(|character| character.locatable)
        );
        assert_eq!(
            walked.characters[4].baseline_origin,
            Point { x: 100.0, y: 100.0 }
        );
    }

    #[test]
    fn form_depth_limit_allows_64_levels_and_skips_the_65th() {
        let accepted = walk_form_chain(MAX_FORM_DEPTH);
        assert_eq!(text_of(&accepted), "MIMUS");
        assert!(accepted.recoveries.is_empty());

        let skipped = walk_form_chain(MAX_FORM_DEPTH + 1);
        assert!(skipped.characters.is_empty());
        assert_eq!(
            skipped.recoveries,
            BTreeSet::from([RecoveryKind::FormDepthExceeded])
        );
    }

    #[test]
    fn production_walk_serializes_non_utf8_font_names_without_losing_bytes() {
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
        let walked = walk_page(&document, page_id).unwrap();
        assert_eq!(text_of(&walked), "MIMUS");
        assert!(
            walked
                .characters
                .iter()
                .all(|character| character.font.resource_name == "#FF")
        );
    }
}
