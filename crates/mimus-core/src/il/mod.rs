use serde::{Deserialize, Serialize};

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
}

impl Paragraph {
    #[must_use]
    pub fn source_text(&self) -> String {
        match &self.text {
            TextCarrier::Chars { chars } => {
                chars.iter().filter_map(|value| value.unicode).collect()
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
    pub font: FontRef,
    pub font_size: f64,
    pub baseline_origin: Point,
    pub r#box: Rect,
    pub visual_bbox: Rect,
    pub text_transform: TextTransform,
    pub passthrough: PassthroughRef,
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
                }],
            }],
        };
        let value = serde_json::to_value(document).unwrap();
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["pages"][0]["paragraphs"][0]["text"]["kind"], "chars");
    }
}
