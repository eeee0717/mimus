//! The document intermediate representation.
//!
//! Every stage of the pipeline reads an [`Document`] and writes one back. The
//! IR is serialisable on purpose: dumping it between stages is how regression
//! tests work (see `tests/`), and how a page can be handed to another thread.
//!
//! Coordinates are PDF user space (points, origin bottom-left) throughout.

use serde::{Deserialize, Serialize};

pub mod geom;
pub mod raster;
pub use geom::{Box2, Matrix};
pub use raster::PageRaster;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub pages: Vec<Page>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub number: u32,
    pub media_box: Box2,
    pub crop_box: Box2,
    /// Page `/Rotate`, normalised to 0/90/180/270.
    pub rotation: u16,
    pub layouts: Vec<Layout>,
    pub paragraphs: Vec<Paragraph>,
    /// Characters not yet absorbed into a paragraph.
    pub chars: Vec<Char>,
}

/// A region emitted by layout analysis. `label` is whatever the model produces
/// -- do not hard-code a closed enum, the vocabulary differs per model
/// (DocLayout-YOLO ships 10 labels, PP-DocLayout ships 20+).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layout {
    pub id: u32,
    pub label: String,
    pub confidence: f32,
    pub bbox: Box2,
}

/// Where a character came from. This distinction reaches every downstream
/// stage, so it lives in the type rather than in a side table:
///
/// - [`Native`] chars were lifted out of a content stream. They carry enough
///   state to be re-emitted byte-identically, so typesetting may pass them
///   through untouched (formulas, proper nouns, anything left untranslated).
/// - [`Ocr`] chars were invented from a recognition result. They have no font
///   identity, no colour and no original operators, so they can only ever be
///   *redrawn* -- which is why the OCR path covers the source with a filled
///   rectangle instead of trying to edit it.
///
/// [`Native`]: CharSource::Native
/// [`Ocr`]: CharSource::Ocr
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CharSource {
    Native {
        /// Nesting depth / identity of the enclosing Form XObject, 0 for page.
        xobj_id: u32,
        /// Position in the original drawing order, for z-order restoration.
        render_order: u32,
        /// Verbatim graphics-state operators, replayed on output. Anything the
        /// parser does not model is preserved here rather than dropped.
        passthrough: Vec<u8>,
    },
    Ocr {
        /// The detection box this character was decoded from.
        line_id: u32,
        confidence: f32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Char {
    pub unicode: char,
    /// Metric box, from font ascent/descent. Used for line grouping.
    pub bbox: Box2,
    /// Ink box, the glyph's actual extent. Used for layout IoU.
    pub visual_bbox: Box2,
    pub style: Style,
    pub source: CharSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    /// Resource-dictionary key (`/F1`), not the human font name.
    pub font_id: Option<String>,
    pub size: f32,
    pub vertical: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paragraph {
    pub bbox: Box2,
    /// Label of the layout this paragraph was attributed to.
    pub layout_label: Option<String>,
    pub layout_id: Option<u32>,
    pub parts: Vec<Part>,
}

/// The hinge of the whole design: content that already has coordinates and
/// content that is still just text coexist in one paragraph. Typesetting's job
/// is to turn every [`Part::Translated`] into [`Part::Line`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Part {
    /// A physical line of original characters.
    Line { bbox: Box2, chars: Vec<Char> },
    /// A formula, passed through verbatim.
    Formula { bbox: Box2, chars: Vec<Char> },
    /// Translated text with no positions yet.
    Translated { text: String, style: Style },
}

impl Document {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}
