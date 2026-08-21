//! MuPDF `mutool` —— 墨迹盒 + baseline origin + 绘制顺序，外加页面框读取。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::geom::{PageFrame, Rect};
use crate::oracle::xml::{Item, scan};
use crate::oracle::{ParsedBlock, ParsedPage};
use crate::proc;
use crate::text;

/// `mutool pages` 报告的一页页面框。这是**独立于 manifest** 的页面几何来源。
#[derive(Debug, Clone, PartialEq)]
pub struct PageBoxes {
    pub media_box: [f64; 4],
    pub crop_box: Option<[f64; 4]>,
    pub rotate: i32,
}

/// 读取逐页的 MediaBox / CropBox / Rotate。
pub fn pages(pdf: &Path) -> Result<Vec<PageBoxes>> {
    let args = ["pages".to_string(), pdf.display().to_string()];
    let out =
        proc::run("mutool", &args, Path::new("."), &BTreeMap::new())?.context("mutool 未安装")?;
    if !out.success() {
        bail!("mutool pages 失败：{}", out.combined);
    }
    parse_pages(&out.combined)
}

fn parse_pages(xml: &str) -> Result<Vec<PageBoxes>> {
    // `mutool pages` 的输出不是良构 XML（顶层有 `file.pdf:` 这样的裸文本行），
    // 先剥掉非标签行再交给 XML 层。
    let body: String = xml
        .lines()
        .filter(|l| l.trim_start().starts_with('<'))
        .collect::<Vec<_>>()
        .join("\n");
    let items = scan(&format!("<root>{body}</root>"))?;

    let mut out: Vec<PageBoxes> = Vec::new();
    for item in &items {
        let Item::Start(tag) = item else { continue };
        match tag.name.as_str() {
            "page" => out.push(PageBoxes {
                media_box: [0.0; 4],
                crop_box: None,
                rotate: 0,
            }),
            "MediaBox" | "CropBox" => {
                let boxed = [tag.f64("l")?, tag.f64("b")?, tag.f64("r")?, tag.f64("t")?];
                let page = out
                    .last_mut()
                    .context("mutool pages 在 <page> 之外报告了页面框")?;
                if tag.name == "MediaBox" {
                    page.media_box = boxed;
                } else {
                    page.crop_box = Some(boxed);
                }
            }
            "Rotate" => {
                let page = out
                    .last_mut()
                    .context("mutool pages 在 <page> 之外报告了 Rotate")?;
                page.rotate = tag.f64("v")? as i32;
            }
            _ => {}
        }
    }
    Ok(out)
}

/// 提取每一页的文本块。块的先后是 **content stream 的绘制顺序**——MuPDF 的
/// structured-text 设备按绘制调用顺序累积，不做二维重排。这是 ORDER-01
/// 「顺序变体的绘制序确实不同」那一半断言的唯一来源。
pub fn blocks(pdf: &Path, frames: &[PageFrame]) -> Result<Vec<ParsedPage>> {
    let xml = dump_stext(pdf)?;
    parse_stext(&xml, frames)
}

fn dump_stext(pdf: &Path) -> Result<String> {
    let args: Vec<String> = ["draw", "-q", "-F", "stext", "-o", "-"]
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once(pdf.display().to_string()))
        .collect();
    let out =
        proc::run("mutool", &args, Path::new("."), &BTreeMap::new())?.context("mutool 未安装")?;
    if !out.success() {
        bail!("mutool draw -F stext 失败：{}", out.combined);
    }
    Ok(out.combined)
}

fn parse_stext(xml: &str, frames: &[PageFrame]) -> Result<Vec<ParsedPage>> {
    let items = scan(xml)?;

    let mut pages: Vec<ParsedPage> = Vec::new();
    let mut frame: Option<PageFrame> = None;
    let mut lines: Vec<String> = Vec::new();
    let mut ink: Option<Rect> = None;
    let mut baseline: Option<(f64, f64)> = None;

    for item in &items {
        match item {
            Item::Start(tag) if tag.name == "page" => {
                let index = pages.len();
                let f = *frames
                    .get(index)
                    .with_context(|| format!("mutool 报了第 {} 页，但 manifest 没有", index + 1))?;
                frame = Some(f);
                pages.push(ParsedPage {
                    index,
                    viewer_size: (tag.f64("width")?, tag.f64("height")?),
                    blocks: Vec::new(),
                });
            }
            Item::Start(tag) if tag.name == "block" => {
                lines.clear();
                ink = None;
                baseline = None;
            }
            Item::Start(tag) if tag.name == "line" => {
                lines.push(tag.attr("text").unwrap_or_default().to_string());
            }
            Item::Start(tag) if tag.name == "char" => {
                // 空格的 quad 是退化的（上下边重合），并入墨迹盒会把行高压扁。
                let q = tag.numbers("quad")?;
                if q.len() != 8 {
                    bail!("<char> 的 quad 应有 8 个数，实际 {}", q.len());
                }
                let (x0, x1) = (q[0].min(q[6]), q[2].max(q[4]));
                let (y0, y1) = (q[1].min(q[3]), q[5].max(q[7]));
                if (y1 - y0).abs() > f64::EPSILON {
                    let f = frame.context("mutool 输出里出现了不在 <page> 内的 <char>")?;
                    let r = f.rect_to_page(x0, y0, x1, y1);
                    ink = Some(match ink {
                        Some(acc) => acc.union(r),
                        None => r,
                    });
                }
                if baseline.is_none() {
                    baseline = Some((tag.f64("x")?, tag.f64("y")?));
                }
            }
            Item::End(name) if name == "block" => {
                let f = frame.context("mutool 输出里出现了不在 <page> 内的 <block>")?;
                let Some(rect) = ink.take() else {
                    // 纯空白块：没有墨迹就没有几何可裁定，直接跳过。
                    lines.clear();
                    baseline = None;
                    continue;
                };
                let page = pages.last_mut().expect("<block> 必在 <page> 之后");
                page.blocks.push(ParsedBlock {
                    text: text::join_lines(&lines),
                    rect,
                    baseline_origin: baseline.take().map(|(x, y)| f.to_page(x, y)),
                });
                lines.clear();
            }
            _ => {}
        }
    }

    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEXT: &str = r#"<?xml version="1.0"?>
<document filename="two.pdf">
<page id="page1" width="400" height="300">
<block bbox="30 29.6 192.2 65.8" justify="unknown">
<line bbox="30 29.6 191.6 38.0" wmode="0" dir="1 0" flags="0" text="Alpha sur&#xad;">
<char c="A" quad="30 30 36.2 30 30 35.9 36.2 35.9" x="30" y="35.9"/>
<char c=" " quad="52 35.9 54 35.9 52 35.9 54 35.9" x="52" y="35.9"/>
<char c="p" quad="38.6 31.9 43.3 31.9 38.6 38.0 43.3 38.0" x="38.6" y="35.9"/>
</line>
<line bbox="30 41 120 51" text="vive">
<char c="v" quad="30 42 36 42 30 50 36 50" x="30" y="49"/>
</line>
</block>
<block bbox="210 29.6 370 65.8">
<line bbox="210 29.6 370 38" text="Beta">
<char c="B" quad="210 30 216 30 210 36 216 36" x="210" y="35.9"/>
</line>
</block>
</page>
</document>"#;

    fn frames() -> Vec<PageFrame> {
        vec![PageFrame::new([0.0, 0.0, 400.0, 300.0], 0).unwrap()]
    }

    #[test]
    fn extracts_blocks_in_draw_order() {
        let pages = parse_stext(STEXT, &frames()).unwrap();
        let texts: Vec<&str> = pages[0].blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, vec!["Alpha survive", "Beta"]);
    }

    #[test]
    fn the_block_box_is_the_union_of_glyph_ink() {
        let pages = parse_stext(STEXT, &frames()).unwrap();
        let r = pages[0].blocks[0].rect;
        assert_eq!(r.x0, 30.0);
        assert_eq!(r.x1, 43.3);
        // 观看空间 y 从 30 到 50 → 页面空间 250..270。
        assert!((r.y0 - 250.0).abs() < 1e-9, "{r:?}");
        assert!((r.y1 - 270.0).abs() < 1e-9, "{r:?}");
    }

    #[test]
    fn degenerate_space_quads_do_not_widen_the_ink_box() {
        // 空格的 quad 上下边重合；若并入墨迹盒，x1 会被拉到 54。
        let pages = parse_stext(STEXT, &frames()).unwrap();
        assert_eq!(pages[0].blocks[0].rect.x1, 43.3);
    }

    #[test]
    fn the_baseline_comes_from_the_first_char() {
        let pages = parse_stext(STEXT, &frames()).unwrap();
        let p = pages[0].blocks[0].baseline_origin.unwrap();
        assert_eq!(p.x, 30.0);
        assert!((p.y - (300.0 - 35.9)).abs() < 1e-9);
    }

    #[test]
    fn parses_mutool_pages_output_including_its_bare_header_line() {
        let out = "two.pdf:\n<page pagenum=\"1\">\n<MediaBox l=\"0\" b=\"0\" r=\"400\" t=\"300\" />\n<Rotate v=\"90\" />\n</page>\n";
        let pages = parse_pages(out).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].media_box, [0.0, 0.0, 400.0, 300.0]);
        assert_eq!(pages[0].crop_box, None);
        assert_eq!(pages[0].rotate, 90);
    }

    #[test]
    fn reads_a_crop_box_when_present() {
        let out = "x.pdf:\n<page pagenum=\"1\">\n<MediaBox l=\"0\" b=\"0\" r=\"400\" t=\"300\" />\n<CropBox l=\"10\" b=\"10\" r=\"390\" t=\"290\" />\n</page>\n";
        let pages = parse_pages(out).unwrap();
        assert_eq!(pages[0].crop_box, Some([10.0, 10.0, 390.0, 290.0]));
    }
}
