//! poppler `pdftotext -bbox-layout` —— 度量盒 + 阅读顺序。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::geom::PageFrame;
use crate::oracle::xml::{Item, scan};
use crate::oracle::{ParsedBlock, ParsedPage};
use crate::proc;
use crate::text;

/// 提取每一页的文本块。块的先后是 poppler **版面分析后的阅读顺序**——它不依赖
/// content stream 的绘制次序，这正是 ORDER-01 要拿来做对照的那一路信号。
pub fn blocks(pdf: &Path, frames: &[PageFrame]) -> Result<Vec<ParsedPage>> {
    let xml = dump(pdf)?;
    parse(&xml, frames)
}

fn dump(pdf: &Path) -> Result<String> {
    let args = vec![
        "-bbox-layout".to_string(),
        pdf.display().to_string(),
        "-".to_string(),
    ];
    let out = proc::run("pdftotext", &args, Path::new("."), &BTreeMap::new())?
        .context("pdftotext 未安装")?;
    if !out.success() {
        bail!("pdftotext -bbox-layout 失败：{}", out.diagnostics());
    }
    Ok(out.stdout_text()?.to_string())
}

fn parse(xml: &str, frames: &[PageFrame]) -> Result<Vec<ParsedPage>> {
    let items = scan(xml)?;

    let mut pages: Vec<ParsedPage> = Vec::new();
    let mut frame: Option<PageFrame> = None;
    // poppler 的层级是 page > flow > block > line > word。
    let mut lines: Vec<String> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut block_rect: Option<[f64; 4]> = None;

    for item in &items {
        match item {
            Item::Start(tag) if tag.name == "page" => {
                let index = pages.len();
                let f = *frames.get(index).with_context(|| {
                    format!("poppler 报了第 {} 页，但 manifest 没有", index + 1)
                })?;
                frame = Some(f);
                pages.push(ParsedPage {
                    index,
                    viewer_size: (tag.f64("width")?, tag.f64("height")?),
                    blocks: Vec::new(),
                });
            }
            Item::Start(tag) if tag.name == "block" => {
                block_rect = Some([
                    tag.f64("xMin")?,
                    tag.f64("yMin")?,
                    tag.f64("xMax")?,
                    tag.f64("yMax")?,
                ]);
                lines.clear();
            }
            Item::Start(tag) if tag.name == "word" => words.push(tag.text.clone()),
            Item::End(name) if name == "line" => {
                lines.push(words.join(" "));
                words.clear();
            }
            Item::End(name) if name == "block" => {
                let (Some(f), Some(r)) = (frame, block_rect.take()) else {
                    bail!("poppler 输出里出现了不在 <page> 内的 <block>");
                };
                let page = pages.last_mut().expect("<block> 必在 <page> 之后");
                page.blocks.push(ParsedBlock {
                    text: text::join_lines(&lines),
                    rect: f.rect_to_page(r[0], r[1], r[2], r[3]),
                    baseline_origin: None,
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

    const SAMPLE: &str = r#"<html><body><doc>
  <page width="400.000000" height="300.000000">
    <flow>
      <block xMin="30.000000" yMin="27.876000" xMax="191.673100" yMax="67.902000">
        <line xMin="30" yMin="27.876" xMax="191.6731" yMax="38.136">
          <word xMin="30" yMin="27.876" xMax="52.257" yMax="38.136">Alpha</word>
          <word xMin="55.885" yMin="27.876" xMax="75.757" yMax="38.136">sur&#xad;</word>
        </line>
        <line xMin="30" yMin="41" xMax="120" yMax="51">
          <word xMin="30" yMin="41" xMax="60" yMax="51">vive</word>
        </line>
      </block>
      <block xMin="210.000000" yMin="27.876000" xMax="370.000000" yMax="67.902000">
        <line xMin="210" yMin="27.876" xMax="370" yMax="38.136">
          <word xMin="210" yMin="27.876" xMax="240" yMax="38.136">Beta</word>
        </line>
      </block>
    </flow>
  </page>
</doc></body></html>"#;

    fn frames() -> Vec<PageFrame> {
        vec![PageFrame::new([0.0, 0.0, 400.0, 300.0], 0).unwrap()]
    }

    #[test]
    fn extracts_blocks_in_document_order() {
        let pages = parse(SAMPLE, &frames()).unwrap();
        assert_eq!(pages.len(), 1);
        let texts: Vec<&str> = pages[0].blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, vec!["Alpha survive", "Beta"]);
    }

    #[test]
    fn converts_the_block_box_into_page_space() {
        let pages = parse(SAMPLE, &frames()).unwrap();
        let r = pages[0].blocks[0].rect;
        assert_eq!(r.x0, 30.0);
        assert_eq!(r.x1, 191.6731);
        // y 翻转：观看空间 yMin=27.876 是页面空间的上边。
        assert!((r.y1 - (300.0 - 27.876)).abs() < 1e-9);
        assert!((r.y0 - (300.0 - 67.902)).abs() < 1e-9);
    }

    #[test]
    fn reports_the_viewer_page_size() {
        let pages = parse(SAMPLE, &frames()).unwrap();
        assert_eq!(pages[0].viewer_size, (400.0, 300.0));
    }

    #[test]
    fn poppler_never_claims_to_know_a_baseline() {
        let pages = parse(SAMPLE, &frames()).unwrap();
        assert!(pages[0].blocks.iter().all(|b| b.baseline_origin.is_none()));
    }

    #[test]
    fn fails_loudly_when_the_pdf_has_more_pages_than_the_manifest() {
        let two_pages = SAMPLE.replace("</doc>", "<page width=\"1\" height=\"1\"/></doc>");
        assert!(parse(&two_pages, &frames()).is_err());
    }
}
