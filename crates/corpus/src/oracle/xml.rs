//! 两个解析器的输出都是 XML，这里是共用的最小读取层。

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use quick_xml::events::Event;

/// 一个开始标签：名字 + 属性表。
pub struct Tag {
    pub name: String,
    pub attrs: BTreeMap<String, String>,
    /// 该标签内的文本（仅 `<word>text</word>` 这类用得上）。
    pub text: String,
}

impl Tag {
    pub fn attr(&self, key: &str) -> Result<&str> {
        self.attrs
            .get(key)
            .map(String::as_str)
            .with_context(|| format!("<{}> 缺少属性 {key}", self.name))
    }

    pub fn f64(&self, key: &str) -> Result<f64> {
        let raw = self.attr(key)?;
        raw.parse()
            .with_context(|| format!("<{}> 的 {key}={raw:?} 不是数字", self.name))
    }

    /// 解析形如 `"x0 y0 x1 y1"` 的空白分隔数字串。
    pub fn numbers(&self, key: &str) -> Result<Vec<f64>> {
        let raw = self.attr(key)?;
        raw.split_whitespace()
            .map(|t| {
                t.parse::<f64>()
                    .with_context(|| format!("<{}> 的 {key} 含非数字 {t:?}", self.name))
            })
            .collect()
    }
}

/// 事件流中的一项。
pub enum Item {
    Start(Tag),
    End(String),
}

/// 把 XML 拍平成「开始标签（带其直接文本）/ 结束标签」的线性事件流。
///
/// 两个解析器的输出都是浅层且规整的，用不上通用 DOM；线性流反而让「保持文档
/// 顺序」这件事——恰恰是 mutool 绘制序断言的依据——变得显式。
pub fn scan(xml: &str) -> Result<Vec<Item>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = false;

    let mut items = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => items.push(Item::Start(tag(&e)?)),
            Ok(Event::Empty(e)) => {
                let t = tag(&e)?;
                let name = t.name.clone();
                items.push(Item::Start(t));
                items.push(Item::End(name));
            }
            Ok(Event::End(e)) => {
                items.push(Item::End(
                    String::from_utf8_lossy(e.name().as_ref()).into_owned(),
                ));
            }
            Ok(Event::Text(e)) => {
                let text = e.decode().context("文本节点无法解码")?;
                push_text(&mut items, &text);
            }
            // quick-xml 把 `&#xad;` / `&amp;` 这类引用拆成独立事件。忽略它们
            // 会静默丢字——poppler 正是用 `&#xad;` 表示断行处的软连字符，丢了
            // 之后「sur」与「vive」就会被拼成两个词。
            Ok(Event::GeneralRef(e)) => {
                let name = e.decode().context("实体引用无法解码")?;
                let resolved = resolve_reference(&name)
                    .with_context(|| format!("无法解析实体引用 &{name};"))?;
                push_text(&mut items, &resolved);
            }
            Ok(_) => {}
            Err(e) => bail!("XML 解析失败（偏移 {}）：{e}", reader.buffer_position()),
        }
    }
    Ok(items)
}

fn push_text(items: &mut [Item], text: &str) {
    if let Some(Item::Start(last)) = items.last_mut() {
        last.text.push_str(text);
    }
}

/// 解析 `&#38;` / `&#x26;` 的字符引用与五个预定义实体。
///
/// 两个解析器的输出里只会出现这些；碰到别的一律报错而不是当成空串——
/// 静默吞掉一个不认识的引用等于静默改写被裁定的文本。
fn resolve_reference(name: &str) -> Option<String> {
    if let Some(digits) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        return u32::from_str_radix(digits, 16)
            .ok()
            .and_then(char::from_u32)
            .map(String::from);
    }
    if let Some(digits) = name.strip_prefix('#') {
        return digits
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(String::from);
    }
    match name {
        "amp" => Some("&".into()),
        "lt" => Some("<".into()),
        "gt" => Some(">".into()),
        "quot" => Some("\"".into()),
        "apos" => Some("'".into()),
        _ => None,
    }
}

fn tag(e: &quick_xml::events::BytesStart) -> Result<Tag> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut attrs = BTreeMap::new();
    for attr in e.attributes() {
        let attr = attr.with_context(|| format!("<{name}> 的属性无法解析"))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .with_context(|| format!("<{name}> 的属性 {key} 无法解码"))?
            .into_owned();
        attrs.insert(key, value);
    }
    Ok(Tag {
        name,
        attrs,
        text: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_document_order_and_pairs_text_with_its_tag() {
        let items = scan("<a><w x=\"1\">hi</w><w x=\"2\">yo</w></a>").unwrap();
        let words: Vec<(String, String)> = items
            .iter()
            .filter_map(|i| match i {
                Item::Start(t) if t.name == "w" => {
                    Some((t.attr("x").unwrap().to_string(), t.text.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            words,
            vec![("1".into(), "hi".into()), ("2".into(), "yo".into())]
        );
    }

    #[test]
    fn self_closing_tags_emit_both_a_start_and_an_end() {
        let items = scan("<a><c q=\"1 2 3 4\"/></a>").unwrap();
        assert_eq!(items.len(), 4);
        match &items[1] {
            Item::Start(t) => assert_eq!(t.numbers("q").unwrap(), vec![1.0, 2.0, 3.0, 4.0]),
            Item::End(_) => panic!("期望 <c> 的开始事件"),
        }
    }

    #[test]
    fn decodes_entity_references_in_text_nodes() {
        let items = scan("<w>sur&#xad;</w>").unwrap();
        match &items[0] {
            Item::Start(t) => assert_eq!(t.text, "sur\u{ad}"),
            Item::End(_) => panic!("期望 <w> 的开始事件"),
        }
    }

    #[test]
    fn decodes_entity_references_in_attributes() {
        // poppler 与 mutool 都会把软连字符写成 &#xad;。
        let items = scan("<l text=\"sur&#xad;\"/>").unwrap();
        match &items[0] {
            Item::Start(t) => assert_eq!(t.attr("text").unwrap(), "sur\u{ad}"),
            Item::End(_) => panic!("期望 <l> 的开始事件"),
        }
    }

    #[test]
    fn tolerates_the_xhtml_doctype_poppler_emits() {
        let xml = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"x.dtd\">\
                   <html><doc><page width=\"1\"/></doc></html>";
        let items = scan(xml).unwrap();
        assert!(
            items
                .iter()
                .any(|i| matches!(i, Item::Start(t) if t.name == "page"))
        );
    }

    #[test]
    fn reports_a_missing_attribute_instead_of_defaulting_it() {
        let items = scan("<c/>").unwrap();
        match &items[0] {
            Item::Start(t) => assert!(t.f64("q").is_err()),
            Item::End(_) => panic!("期望 <c> 的开始事件"),
        }
    }
}
