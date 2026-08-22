//! MuPDF SVG outline oracle for hand-written visual bounding boxes.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use svgtypes::{SimplePathSegment, SimplifyingPathParser, TransformListParser, TransformListToken};

use crate::geom::Rect;
use crate::oracle::xml::{Item, scan};
use crate::proc;

#[derive(Debug, Clone, PartialEq)]
pub struct OutlineGlyph {
    pub text: String,
    pub rect: Rect,
}

pub fn glyphs(pdf: &Path, page_index: usize) -> Result<Vec<OutlineGlyph>> {
    let args = vec![
        "draw".to_string(),
        "-q".to_string(),
        "-F".to_string(),
        "svg".to_string(),
        "-o".to_string(),
        "-".to_string(),
        pdf.display().to_string(),
        (page_index + 1).to_string(),
    ];
    let output =
        proc::run("mutool", &args, Path::new("."), &BTreeMap::new())?.context("mutool 未安装")?;
    if !output.success() {
        bail!("mutool draw -F svg 失败：{}", output.diagnostics());
    }
    parse_svg(output.stdout_text()?)
}

fn parse_svg(svg: &str) -> Result<Vec<OutlineGlyph>> {
    let items = scan(svg)?;
    let mut paths: BTreeMap<String, String> = BTreeMap::new();
    let mut glyphs = Vec::new();

    for item in &items {
        let Item::Start(tag) = item else { continue };
        if tag.name == "path" {
            if let Some(id) = tag.attrs.get("id") {
                paths.insert(id.clone(), tag.attr("d")?.to_string());
            }
            continue;
        }
        if tag.name != "use" {
            continue;
        }
        let Some(text) = tag.attrs.get("data-text") else {
            continue;
        };
        if text.chars().all(char::is_whitespace) {
            continue;
        }
        let href = tag
            .attrs
            .get("xlink:href")
            .or_else(|| tag.attrs.get("href"))
            .context("MuPDF SVG text <use> has no href")?;
        let id = href
            .strip_prefix('#')
            .context("MuPDF SVG glyph href is not a local fragment")?;
        let path = paths
            .get(id)
            .with_context(|| format!("MuPDF SVG glyph references missing path #{id}"))?;
        let mut transforms = TransformListParser::from(tag.attr("transform")?);
        let matrix = match transforms
            .next()
            .transpose()
            .context("parse MuPDF SVG transform")?
        {
            Some(TransformListToken::Matrix { a, b, c, d, e, f }) => [a, b, c, d, e, f],
            Some(other) => bail!("MuPDF SVG glyph transform is not a matrix: {other:?}"),
            None => bail!("MuPDF SVG glyph has an empty transform"),
        };
        if transforms.next().is_some() {
            bail!("MuPDF SVG glyph has more than one transform");
        }
        glyphs.push(OutlineGlyph {
            text: text.clone(),
            rect: path_bounds(path, matrix)?,
        });
    }
    Ok(glyphs)
}

fn path_bounds(path: &str, transform: [f64; 6]) -> Result<Rect> {
    let point = |x: f64, y: f64| -> (f64, f64) {
        (
            transform[0] * x + transform[2] * y + transform[4],
            transform[1] * x + transform[3] * y + transform[5],
        )
    };
    let mut bounds: Option<Rect> = None;
    let mut current = (0.0, 0.0);
    let mut subpath = (0.0, 0.0);

    for segment in SimplifyingPathParser::from(path) {
        match segment.context("parse MuPDF SVG path")? {
            SimplePathSegment::MoveTo { x, y } => {
                current = point(x, y);
                subpath = current;
            }
            SimplePathSegment::LineTo { x, y } => {
                let end = point(x, y);
                include_curve(&mut bounds, [current, end], &[]);
                current = end;
            }
            SimplePathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let controls = [point(x1, y1), point(x2, y2)];
                let end = point(x, y);
                include_curve(
                    &mut bounds,
                    [current, end],
                    &cubic_extrema(current, controls, end),
                );
                current = end;
            }
            SimplePathSegment::Quadratic { x1, y1, x, y } => {
                let control = point(x1, y1);
                let end = point(x, y);
                include_curve(
                    &mut bounds,
                    [current, end],
                    &quadratic_extrema(current, control, end),
                );
                current = end;
            }
            SimplePathSegment::ClosePath => {
                include_curve(&mut bounds, [current, subpath], &[]);
                current = subpath;
            }
        }
    }
    bounds.context("MuPDF SVG path contains no drawable segment")
}

fn include_curve(bounds: &mut Option<Rect>, ends: [(f64, f64); 2], extrema: &[(f64, f64)]) {
    for (x, y) in ends.into_iter().chain(extrema.iter().copied()) {
        let point = Rect::new(x, y, x, y);
        *bounds = Some(match *bounds {
            Some(current) => current.union(point),
            None => point,
        });
    }
}

fn cubic_extrema(start: (f64, f64), controls: [(f64, f64); 2], end: (f64, f64)) -> Vec<(f64, f64)> {
    let mut parameters = Vec::new();
    for (p0, p1, p2, p3) in [
        (start.0, controls[0].0, controls[1].0, end.0),
        (start.1, controls[0].1, controls[1].1, end.1),
    ] {
        let a = -p0 + 3.0 * p1 - 3.0 * p2 + p3;
        let b = 3.0 * p0 - 6.0 * p1 + 3.0 * p2;
        let c = -3.0 * p0 + 3.0 * p1;
        parameters.extend(quadratic_roots(3.0 * a, 2.0 * b, c));
    }
    parameters.sort_by(f64::total_cmp);
    parameters.dedup_by(|left, right| (*left - *right).abs() < f64::EPSILON);
    parameters
        .into_iter()
        .filter(|t| *t > 0.0 && *t < 1.0)
        .map(|t| {
            (
                cubic_at(start.0, controls[0].0, controls[1].0, end.0, t),
                cubic_at(start.1, controls[0].1, controls[1].1, end.1, t),
            )
        })
        .collect()
}

fn quadratic_extrema(start: (f64, f64), control: (f64, f64), end: (f64, f64)) -> Vec<(f64, f64)> {
    let mut parameters = Vec::new();
    for (p0, p1, p2) in [(start.0, control.0, end.0), (start.1, control.1, end.1)] {
        let denominator = p0 - 2.0 * p1 + p2;
        if denominator.abs() > f64::EPSILON {
            parameters.push((p0 - p1) / denominator);
        }
    }
    parameters.sort_by(f64::total_cmp);
    parameters.dedup_by(|left, right| (*left - *right).abs() < f64::EPSILON);
    parameters
        .into_iter()
        .filter(|t| *t > 0.0 && *t < 1.0)
        .map(|t| {
            (
                quadratic_at(start.0, control.0, end.0, t),
                quadratic_at(start.1, control.1, end.1, t),
            )
        })
        .collect()
}

fn quadratic_roots(a: f64, b: f64, c: f64) -> Vec<f64> {
    if a.abs() <= f64::EPSILON {
        return if b.abs() <= f64::EPSILON {
            Vec::new()
        } else {
            vec![-c / b]
        };
    }
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 {
        return Vec::new();
    }
    let root = discriminant.sqrt();
    vec![(-b - root) / (2.0 * a), (-b + root) / (2.0 * a)]
}

fn cubic_at(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

fn quadratic_at(p0: f64, p1: f64, p2: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    u * u * p0 + 2.0 * u * t * p1 + t * t * p2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_curve_extrema_after_the_svg_transform() {
        // The cubic reaches y=0.75 at t=0.5; its control points reach y=1.
        // A control-point bbox would therefore fail this known result.
        let bounds = path_bounds("M0 0C0 1 1 1 1 0", [12.0, 0.0, 0.0, -12.0, 72.0, 80.0]).unwrap();
        assert!((bounds.x0 - 72.0).abs() < 1e-9, "{bounds:?}");
        assert!((bounds.x1 - 84.0).abs() < 1e-9, "{bounds:?}");
        assert!((bounds.y0 - 71.0).abs() < 1e-9, "{bounds:?}");
        assert!((bounds.y1 - 80.0).abs() < 1e-9, "{bounds:?}");
    }

    #[test]
    fn resolves_glyph_paths_and_ignores_non_text_artwork() {
        let svg = r##"<svg><defs>
          <path id="font_1_1" d="M0 0H1V1H0Z"/>
          <path id="annotation-icon" d="M0 0H100V100H0Z"/>
        </defs>
        <use data-text="M" xlink:href="#font_1_1" transform="matrix(12,0,0,-12,72,80)"/>
        <use xlink:href="#annotation-icon" transform="matrix(1,0,0,1,0,0)"/>
        </svg>"##;
        let glyphs = parse_svg(svg).unwrap();
        assert_eq!(glyphs.len(), 1);
        assert_eq!(glyphs[0].text, "M");
        assert_eq!(glyphs[0].rect, Rect::new(72.0, 68.0, 84.0, 80.0));
    }
}
