use serde::{Deserialize, Serialize};

use crate::error::{InternalReason, MimusError, Result};

// ADR-0007: 这是 IL 快照版本，不是 event.rs 的 CLI 机器协议版本。
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Document {
    pub schema_version: u32,
    pub pages: Vec<Page>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            pages: Vec::new(),
        }
    }
}

#[must_use]
pub fn snapshot(document: &Document) -> Document {
    document.clone()
}

pub fn canonical_json(document: &Document) -> Result<Vec<u8>> {
    let mut output = serde_json::to_vec_pretty(document).map_err(|error| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            format!("could not serialize canonical IL: {error}"),
        )
    })?;
    output.push(b'\n');
    Ok(output)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Page {
    pub index: usize,
    pub geometry: PageGeometry,
    pub paragraphs: Vec<Paragraph>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PageGeometry {
    pub width: f64,
    pub height: f64,
    pub rotate_degrees: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Paragraph {
    pub reading_order: usize,
    pub bounds: Rect,
    pub text: TextCarrier,
    pub translated_text: Option<String>,
    // ADR-0013 §2: 段级保留的载体。additive 可选字段，IL schema 仍为 1；
    // 未保留的段落序列化结果与加字段之前逐字节相同。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved: Option<PreservedReason>,
}

/// 段级保留的原因（ADR-0014 §4）。保留段的 `translated_text` 恒为 `None`。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreservedReason {
    /// 段内存在无法可信解码为 Unicode 的字符。
    UnreliableUnicode,
    /// 字体对象不可解析，或超出 M1 支持面。
    UnsupportedFont,
    /// 字符 advance 不为正或非有限。
    NonPositiveAdvance,
    /// 文本矩阵退化，字符不可定位。
    Unlocatable,
    /// 译文在最低可读字号下仍无法容纳于原段落框。
    TypesetOverflow,
    /// 段落结构不满足翻译到排版的协议不变量。
    TypesetProtocol,
    /// 翻译后端破坏了公式或富文本占位符协议。
    PlaceholderViolation,
    /// 单段翻译请求失败；普通模式保留原文，strict 模式升级为硬失败。
    TranslationFailure,
}

impl Paragraph {
    #[must_use]
    pub fn source_text(&self) -> String {
        match &self.text {
            TextCarrier::Chars { chars } => {
                let mut output = String::new();
                for character in chars {
                    let Some(unicode) = character.unicode else {
                        continue;
                    };
                    if character.implicit_space_before
                        && !output.ends_with(char::is_whitespace)
                        && !unicode.is_whitespace()
                    {
                        output.push(' ');
                    }
                    output.push(unicode);
                }
                output
            }
        }
    }

    #[must_use]
    pub fn chars(&self) -> &[Char] {
        match &self.text {
            TextCarrier::Chars { chars } => chars,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextCarrier {
    Chars { chars: Vec<Char> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Char {
    pub unicode: Option<char>,
    pub code: u32,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub visible: bool,
    pub font: FontRef,
    pub font_size: f64,
    pub baseline_origin: Point,
    pub r#box: Rect,
    pub visual_bbox: Rect,
    pub text_transform: TextTransform,
    /// Geometry proves a word boundary even though the PDF encoded no space
    /// glyph (including a soft line wrap). The character remains tied to its
    /// original byte span; this flag affects request text only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub implicit_space_before: bool,
    /// Layout ownership is additive in IL v1. Characters without a trustworthy
    /// assignment remain passthrough instead of being promoted to body text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutAssignment>,
    pub passthrough: PassthroughRef,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct LayoutAssignment {
    pub label: LayoutLabel,
    pub reading_order: usize,
    pub bounds: Rect,
    pub source: LayoutSource,
    pub policy: TranslationPolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LayoutSource {
    Model,
    FallbackLine,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TranslationPolicy {
    Translate,
    Passthrough,
}

/// PP-DocLayoutV3's fixed 25-class vocabulary plus the local fallback_line
/// marker. The marker is normalized to its enclosing semantic label before it
/// reaches IL whenever a model region owns it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LayoutLabel {
    Abstract,
    Algorithm,
    AsideText,
    Chart,
    Content,
    DisplayFormula,
    DocTitle,
    FigureTitle,
    Footer,
    FooterImage,
    Footnote,
    FormulaNumber,
    Header,
    HeaderImage,
    Image,
    InlineFormula,
    Number,
    ParagraphTitle,
    Reference,
    ReferenceContent,
    Seal,
    Table,
    Text,
    VerticalText,
    VisionFootnote,
    FallbackLine,
}

impl LayoutLabel {
    #[must_use]
    pub const fn translation_policy(self) -> TranslationPolicy {
        match self {
            Self::Abstract
            | Self::AsideText
            | Self::Content
            | Self::DocTitle
            | Self::FigureTitle
            | Self::Footnote
            | Self::ParagraphTitle
            | Self::Text
            | Self::VisionFootnote
            | Self::FallbackLine => TranslationPolicy::Translate,
            Self::Algorithm
            | Self::Chart
            | Self::DisplayFormula
            | Self::Footer
            | Self::FooterImage
            | Self::FormulaNumber
            | Self::Header
            | Self::HeaderImage
            | Self::Image
            | Self::InlineFormula
            | Self::Number
            | Self::Reference
            | Self::ReferenceContent
            | Self::Seal
            | Self::Table
            | Self::VerticalText => TranslationPolicy::Passthrough,
        }
    }
}

const fn default_true() -> bool {
    true
}

const fn is_false(value: &bool) -> bool {
    !*value
}

const fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontRef {
    pub resource_name: String,
    pub object_number: u32,
    pub generation: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PassthroughRef {
    // #14 只消费 encoded 做 none identity typeset；按区间拼接完整原流属于 #18。
    pub content_object: u32,
    pub byte_start: usize,
    pub byte_end: usize,
    pub encoded: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "degrees", rename_all = "snake_case")]
pub enum TextTransform {
    Upright,
    Rotated(f64),
    Mirrored,
    Skewed(f64),
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

impl Rect {
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            bottom: self.bottom.min(other.bottom),
            right: self.right.max(other.right),
            top: self.top.max(other.top),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn il_json_has_its_own_schema_and_tagged_text_carrier() {
        let document = Document {
            schema_version: SCHEMA_VERSION,
            pages: vec![Page {
                index: 0,
                geometry: PageGeometry {
                    width: 300.0,
                    height: 200.0,
                    rotate_degrees: 0,
                },
                paragraphs: vec![Paragraph {
                    reading_order: 0,
                    bounds: Rect::default(),
                    text: TextCarrier::Chars { chars: Vec::new() },
                    translated_text: None,
                    preserved: None,
                }],
            }],
        };
        let value = serde_json::to_value(&document).unwrap();
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["pages"][0]["paragraphs"][0]["text"]["kind"], "chars");
        let canonical = canonical_json(&document).unwrap();
        assert!(canonical.ends_with(b"\n"));
    }

    #[test]
    fn preserved_is_additive_and_absent_until_a_paragraph_is_preserved() {
        let mut paragraph = Paragraph {
            reading_order: 0,
            bounds: Rect::default(),
            text: TextCarrier::Chars { chars: Vec::new() },
            translated_text: None,
            preserved: None,
        };

        // 未保留的段落不写出该键——既有 IL 消费者看到的字节不变。
        let value = serde_json::to_value(&paragraph).unwrap();
        assert!(value.get("preserved").is_none());

        paragraph.preserved = Some(PreservedReason::UnreliableUnicode);
        let value = serde_json::to_value(&paragraph).unwrap();
        assert_eq!(value["preserved"], "unreliable_unicode");

        // 缺该键的旧快照仍可读回，反序列化得到 None。
        let restored: Paragraph = serde_json::from_str(
            r#"{"reading_order":0,"bounds":{"left":0.0,"bottom":0.0,"right":0.0,"top":0.0},"text":{"kind":"chars","chars":[]},"translated_text":null}"#,
        )
        .unwrap();
        assert_eq!(restored.preserved, None);
    }
}
