//! MuPDF trace text matrices and advances for transformed-text metric boxes.
//!
//! Poppler's bbox-layout output is a useful metric-box oracle for horizontal
//! and quarter-turn text, but it snaps arbitrary rotations and reports a
//! shear-specific word box. MuPDF trace exposes the actual text rendering
//! matrix, glyph origin, and PDF advance, allowing the descriptor ascent and
//! descent read independently by qpdf to be transformed without that loss.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::geom::Rect;
use crate::oracle::xml::{Item, scan};
use crate::proc;

#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    pub unicode: String,
    pub font: String,
    pub trm: [f64; 4],
    pub origin: [f64; 2],
    pub advance: f64,
}

impl Glyph {
    pub fn metric_box(&self, ascent: f64, descent: f64) -> Rect {
        let [a, b, c, d] = self.trm;
        let [x, y] = self.origin;
        let points = [
            (0.0, descent),
            (0.0, ascent),
            (self.advance, descent),
            (self.advance, ascent),
        ];
        let transformed = points.map(|(px, py)| (a * px + c * py + x, b * px + d * py + y));
        Rect::new(
            transformed
                .iter()
                .map(|point| point.0)
                .fold(f64::INFINITY, f64::min),
            transformed
                .iter()
                .map(|point| point.1)
                .fold(f64::INFINITY, f64::min),
            transformed
                .iter()
                .map(|point| point.0)
                .fold(f64::NEG_INFINITY, f64::max),
            transformed
                .iter()
                .map(|point| point.1)
                .fold(f64::NEG_INFINITY, f64::max),
        )
    }
}

pub fn glyphs(pdf: &Path) -> Result<Vec<Glyph>> {
    let args: Vec<String> = ["draw", "-q", "-F", "trace", "-o", "-"]
        .iter()
        .map(|value| value.to_string())
        .chain(std::iter::once(pdf.display().to_string()))
        .collect();
    let output =
        proc::run("mutool", &args, Path::new("."), &BTreeMap::new())?.context("mutool 未安装")?;
    if !output.success() {
        bail!("mutool draw -F trace 失败：{}", output.diagnostics());
    }
    parse(output.stdout_text()?)
}

fn parse(xml: &str) -> Result<Vec<Glyph>> {
    let mut font: Option<String> = None;
    let mut trm: Option<[f64; 4]> = None;
    let mut glyphs = Vec::new();
    for item in scan(xml)? {
        match item {
            Item::Start(tag) if tag.name == "span" => {
                font = Some(tag.attr("font")?.to_string());
                trm = Some(tag.numbers("trm")?.try_into().map_err(|values: Vec<f64>| {
                    anyhow::anyhow!("mutool trace trm has {} values", values.len())
                })?);
            }
            Item::Start(tag) if tag.name == "g" => glyphs.push(Glyph {
                unicode: tag.attr("unicode")?.to_string(),
                font: font
                    .clone()
                    .context("trace glyph appeared outside a span")?,
                trm: trm.context("trace glyph has no text rendering matrix")?,
                origin: [tag.f64("x")?, tag.f64("y")?],
                advance: tag.f64("adv")?,
            }),
            Item::End(name) if name == "span" => {
                font = None;
                trm = None;
            }
            _ => {}
        }
    }
    Ok(glyphs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_glyph_and_transforms_its_metric_box() {
        let glyph = parse(
            r#"<page><fill_text><span font="MIMUSI+DejaVuSans" trm="0 12 -12 0"><g unicode="M" x="100" y="700" adv=".863"/></span></fill_text></page>"#,
        )
        .unwrap()
        .remove(0);

        assert_eq!(glyph.unicode, "M");
        assert_eq!(
            glyph.metric_box(0.928, -0.236).to_array(),
            [88.864, 700.0, 102.832, 710.356]
        );
    }
}
