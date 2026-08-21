//! Text detection and recognition.
//!
//! Target models are the official PP-OCRv6 ONNX exports on the Hugging Face
//! Hub (`PaddlePaddle/PP-OCRv6_{tiny,small,medium}_{det,rec}_onnx`), so no
//! paddle2onnx step is needed here.
//!
//! Inference is the easy half. The work is the post-processing that lives
//! *outside* the ONNX graph:
//!
//! - detection is a DBNet-family segmentation head, so the graph emits a
//!   probability map, not boxes: threshold, trace contours, unclip the polygon
//!   (Vatti offset), then take the minimum-area rectangle;
//! - recognition is CTC: argmax, collapse repeats, drop blanks, index a
//!   character dictionary.
//!
//! Port the thresholds from PaddleOCR's `DBPostProcess` defaults before
//! inventing your own; they are load-bearing.

use mimus_ir::{Box2, PageRaster};

pub struct TextLine {
    pub bbox: Box2,
    pub text: String,
    pub confidence: f32,
}

pub trait TextDetector {
    fn detect(&self, page: &PageRaster) -> anyhow::Result<Vec<Box2>>;
}

pub trait TextRecognizer {
    fn recognize(&self, crops: &[(Vec<u8>, u32, u32)]) -> anyhow::Result<Vec<(String, f32)>>;
}
