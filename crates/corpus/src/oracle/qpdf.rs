//! qpdf `--check` —— §2.8 步骤 2 的合法性核验。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::proc;

/// 结构检查的结论。
pub struct CheckResult {
    pub passed: bool,
    pub report: String,
}

pub struct Document {
    raw: Value,
}

impl Document {
    pub(crate) fn parse(json: &str) -> Result<Self> {
        let raw: Value = serde_json::from_str(json).context("qpdf JSON 无法解析")?;
        let document = Self { raw };
        document.objects()?;
        Ok(document)
    }

    pub fn load(pdf: &Path) -> Result<Self> {
        let args = vec![
            "--json".to_string(),
            "--json-stream-data=none".to_string(),
            pdf.display().to_string(),
        ];
        let output =
            proc::run("qpdf", &args, Path::new("."), &BTreeMap::new())?.context("qpdf 未安装")?;
        if !output.success() {
            bail!("qpdf --json 失败：{}", output.diagnostics());
        }
        Self::parse(output.stdout_text()?)
    }

    pub fn reference(&self, object: u32, path: &[String]) -> Result<u32> {
        let value = self.value(object, path)?;
        parse_reference(value).with_context(|| {
            format!(
                "object {object} path {} is not an indirect reference",
                path.join("/")
            )
        })
    }

    pub fn optional_reference(&self, object: u32, path: &[String]) -> Result<Option<u32>> {
        self.optional_value(object, path)?
            .map(|value| {
                parse_reference(value).with_context(|| {
                    format!(
                        "object {object} path {} is not an indirect reference",
                        path.join("/")
                    )
                })
            })
            .transpose()
    }

    pub fn optional_references(&self, object: u32, path: &[String]) -> Result<Vec<u32>> {
        let Some(value) = self.optional_value(object, path)? else {
            return Ok(Vec::new());
        };
        value
            .as_array()
            .with_context(|| format!("object {object} path {} is not an array", path.join("/")))?
            .iter()
            .map(parse_reference)
            .collect()
    }

    pub fn page_objects(&self) -> Result<Vec<u32>> {
        self.raw
            .get("pages")
            .and_then(Value::as_array)
            .context("qpdf JSON has no pages array")?
            .iter()
            .map(|page| {
                page.get("object")
                    .context("qpdf page has no object")
                    .and_then(parse_reference)
            })
            .collect()
    }

    pub fn uri_action_count(&self) -> Result<usize> {
        Ok(self
            .objects()?
            .values()
            .filter_map(|entry| entry.get("value"))
            .map(count_uri_actions)
            .sum())
    }

    pub fn value(&self, object: u32, path: &[String]) -> Result<&Value> {
        self.optional_value(object, path)?
            .with_context(|| format!("object {object} path {} is absent", path.join("/")))
    }

    fn optional_value(&self, object: u32, path: &[String]) -> Result<Option<&Value>> {
        if path.is_empty() {
            bail!("object value path must not be empty");
        }
        let Some(mut value) = self.dictionary(object)?.get(&path[0]) else {
            return Ok(None);
        };
        for part in &path[1..] {
            let next = if let Ok(index) = part.parse::<usize>() {
                value
                    .as_array()
                    .with_context(|| format!("object {object} path before {part} is not an array"))?
                    .get(index)
            } else {
                value
                    .as_object()
                    .with_context(|| {
                        format!("object {object} path before {part} is not a dictionary")
                    })?
                    .get(part)
            };
            let Some(next) = next else {
                return Ok(None);
            };
            value = next;
        }
        Ok(Some(value))
    }

    pub fn dictionary(&self, object: u32) -> Result<&Map<String, Value>> {
        self.objects()?
            .get(&format!("obj:{object} 0 R"))
            .with_context(|| format!("qpdf JSON has no object {object} 0 R"))?
            .get("value")
            .and_then(Value::as_object)
            .with_context(|| format!("object {object} is not a dictionary object"))
    }

    pub fn stream_dictionary(&self, object: u32) -> Result<&Map<String, Value>> {
        self.objects()?
            .get(&format!("obj:{object} 0 R"))
            .and_then(|entry| entry.get("stream"))
            .and_then(|stream| stream.get("dict"))
            .and_then(Value::as_object)
            .with_context(|| format!("object {object} is not a stream"))
    }

    pub fn object_numbers(&self) -> Result<Vec<u32>> {
        let mut numbers = Vec::new();
        for key in self.objects()?.keys() {
            let Some(reference) = key.strip_prefix("obj:") else {
                continue;
            };
            let number = reference
                .split_whitespace()
                .next()
                .context("empty qpdf object key")?
                .parse::<u32>()
                .with_context(|| format!("invalid qpdf object key {key}"))?;
            numbers.push(number);
        }
        numbers.sort_unstable();
        Ok(numbers)
    }

    pub fn pdf_version(&self) -> Result<&str> {
        self.raw
            .get("qpdf")
            .and_then(Value::as_array)
            .and_then(|array| array.first())
            .and_then(|header| header.get("pdfversion"))
            .and_then(Value::as_str)
            .context("qpdf JSON has no pdfversion")
    }

    pub fn trailer(&self) -> Result<&Map<String, Value>> {
        self.objects()?
            .get("trailer")
            .and_then(|entry| entry.get("value"))
            .and_then(Value::as_object)
            .context("qpdf JSON has no trailer dictionary")
    }

    pub fn trailer_reference(&self, key: &str) -> Result<u32> {
        let value = self
            .trailer()?
            .get(key)
            .with_context(|| format!("trailer key {key} is absent"))?;
        parse_reference(value).with_context(|| format!("trailer {key} is not a reference"))
    }

    pub fn metadata_streams(&self) -> Result<usize> {
        let mut count = 0;
        for entry in self.objects()?.values() {
            let Some(dictionary) = entry
                .get("stream")
                .and_then(|stream| stream.get("dict"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            if dictionary.get("/Type").and_then(Value::as_str) == Some("/Metadata") {
                count += 1;
            }
        }
        Ok(count)
    }

    fn objects(&self) -> Result<&Map<String, Value>> {
        self.raw
            .get("qpdf")
            .and_then(Value::as_array)
            .and_then(|array| array.get(1))
            .and_then(Value::as_object)
            .context("qpdf JSON has no object table")
    }
}

fn count_uri_actions(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(count_uri_actions).sum(),
        Value::Object(dictionary) => {
            usize::from(
                dictionary.get("/S").and_then(Value::as_str) == Some("/URI")
                    && dictionary.contains_key("/URI"),
            ) + dictionary.values().map(count_uri_actions).sum::<usize>()
        }
        _ => 0,
    }
}

fn parse_reference(value: &Value) -> Result<u32> {
    let text = value.as_str().context("reference is not a JSON string")?;
    let mut parts = text.split_whitespace();
    let object = parts
        .next()
        .context("empty reference")?
        .parse::<u32>()
        .context("invalid reference object number")?;
    let generation = parts.next().context("reference missing generation")?;
    if generation.parse::<u16>().is_err() || parts.next() != Some("R") || parts.next().is_some() {
        bail!("unsupported reference {text:?}");
    }
    Ok(object)
}

pub fn raw_stream(pdf: &Path, object: u32) -> Result<Vec<u8>> {
    let args = vec![
        format!("--show-object={object}"),
        "--raw-stream-data".to_string(),
        pdf.display().to_string(),
    ];
    let output =
        proc::run("qpdf", &args, Path::new("."), &BTreeMap::new())?.context("qpdf 未安装")?;
    raw_stream_output(output, object)
}

fn raw_stream_output(output: proc::Output, object: u32) -> Result<Vec<u8>> {
    if !output.success() {
        bail!("qpdf raw stream {object} 失败：{}", output.diagnostics());
    }
    Ok(output.stdout)
}

/// Return uncompressed object offsets from qpdf's xref view, including non-zero
/// generations so the corpus can exercise generation-preserving references.
pub fn xref_offsets(pdf: &Path) -> Result<BTreeMap<u32, usize>> {
    let args = vec!["--show-xref".to_string(), pdf.display().to_string()];
    let output =
        proc::run("qpdf", &args, Path::new("."), &BTreeMap::new())?.context("qpdf 未安装")?;
    if !output.success() {
        bail!("qpdf --show-xref 失败：{}", output.diagnostics());
    }
    parse_xref_offsets(output.stdout_text()?)
}

fn parse_xref_offsets(report: &str) -> Result<BTreeMap<u32, usize>> {
    let mut offsets = BTreeMap::new();
    for line in report.lines().filter(|line| !line.trim().is_empty()) {
        let (reference, entry) = line
            .split_once(':')
            .with_context(|| format!("qpdf xref 行缺少冒号：{line:?}"))?;
        let (object, generation) = reference
            .split_once('/')
            .with_context(|| format!("qpdf xref 引用格式无效：{reference:?}"))?;
        let object = object
            .parse::<u32>()
            .with_context(|| format!("qpdf xref 对象号无效：{object:?}"))?;
        let _generation = generation
            .parse::<u32>()
            .with_context(|| format!("qpdf xref generation 无效：{generation:?}"))?;
        let entry = entry.trim();
        let Some(offset) = entry.strip_prefix("uncompressed; offset = ") else {
            if entry.starts_with("compressed;") {
                continue;
            }
            bail!("qpdf xref entry 格式无效：{line:?}");
        };
        let offset = offset
            .parse::<usize>()
            .with_context(|| format!("qpdf xref offset 无效：{offset:?}"))?;
        if offsets.insert(object, offset).is_some() {
            bail!("qpdf xref 重复报告 object {object}");
        }
    }
    Ok(offsets)
}

/// 对一份 PDF 跑 `qpdf --check`。
///
/// 注意 qpdf 的退出码分三档：0 无问题、2 有错误、3 只有警告。**警告也算不通过**
/// ——合法 fixture 的判据是「干净」，容忍警告等于给自己留一条以后会被引用成
/// 「本来就这样」的后路。
pub fn check(pdf: &Path) -> Result<CheckResult> {
    let args = vec!["--check".to_string(), pdf.display().to_string()];
    let out = proc::run("qpdf", &args, Path::new("."), &BTreeMap::new())?.context("qpdf 未安装")?;
    Ok(CheckResult {
        passed: out.status == Some(0),
        report: out.combined_text()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_dictionary_and_array_paths_to_an_indirect_reference() {
        let json = r#"{
          "qpdf": [
            {"jsonversion": 2, "pdfversion": "1.7", "maxobjectid": 3},
            {"obj:1 0 R": {"value": {"/Kids": ["3 0 R"]}}}
          ]
        }"#;
        let document = Document::parse(json).unwrap();
        assert_eq!(
            document
                .reference(1, &["/Kids".to_string(), "0".to_string()])
                .unwrap(),
            3
        );
    }

    #[test]
    fn enumerates_actual_page_references_and_uri_actions() {
        let json = r#"{
          "pages": [{"object": "3 0 R"}],
          "qpdf": [
            {"jsonversion": 2, "pdfversion": "1.7", "maxobjectid": 14},
            {
              "obj:1 0 R": {"value": {
                "/Fields": ["14 0 R"],
                "/A": {"/S": "/URI", "/URI": "u:https://example.com"}
              }},
              "obj:3 0 R": {"value": {"/Annots": ["14 0 R"]}},
              "obj:14 0 R": {"value": {"/Subtype": "/Widget"}}
            }
          ]
        }"#;
        let document = Document::parse(json).unwrap();

        assert_eq!(document.page_objects().unwrap(), vec![3]);
        assert_eq!(
            document
                .optional_references(1, &["/Fields".to_string()])
                .unwrap(),
            vec![14]
        );
        assert_eq!(document.uri_action_count().unwrap(), 1);
    }

    #[test]
    fn raw_stream_preserves_non_utf8_font_bytes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let expected = std::fs::read(root.join("corpus/fonts/MimusExact.ttf")).unwrap();
        let output = proc::Output {
            status: Some(0),
            stdout: expected.clone(),
            stderr: Vec::new(),
        };

        let actual = raw_stream_output(output, 7).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn parses_uncompressed_xref_offsets_without_inventing_compressed_offsets() {
        let offsets = parse_xref_offsets(
            "1/0: uncompressed; offset = 15\n2/0: compressed; stream = 9, index = 0\n3/0: uncompressed; offset = 121\n",
        )
        .unwrap();

        assert_eq!(offsets, BTreeMap::from([(1, 15), (3, 121)]));
    }
}
