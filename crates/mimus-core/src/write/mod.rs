use std::collections::BTreeSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use lopdf::{Dictionary, Document, IncrementalDocument, Object, ObjectId, Stream, dictionary};

use crate::error::{InternalReason, IoReason, MimusError, Result};
use crate::il::{FontRef, Point};
use crate::pdf_stream;
use crate::walk::MAX_STREAM_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentSpanReplacement {
    pub content_object: ObjectId,
    pub byte_start: usize,
    pub byte_end: usize,
    pub replacement: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PageRewrite {
    pub page_index: usize,
    pub replacements: Vec<ContentSpanReplacement>,
    pub reused_fonts: Vec<FontRef>,
    pub embedded_fonts: Vec<EmbeddedFont>,
    pub typeset_characters: Vec<TypesetCharacter>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TypesetCharacter {
    pub unicode: char,
    pub baseline_origin: Point,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddedFont {
    pub resource_name: String,
    pub base_font: String,
    pub font_bytes: Vec<u8>,
    pub units_per_em: u16,
    pub ascent: i16,
    pub descent: i16,
    pub cap_height: i16,
    /// Output CID/GID, Unicode scalar, and advance in font units.
    pub glyphs: Vec<(u16, char, u16)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReport {
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub appended_bytes: usize,
    pub content_objects: Vec<ObjectId>,
}

pub(crate) fn build_incremental(
    original_bytes: &[u8],
    original: &Document,
    rewrites: &[PageRewrite],
) -> Result<(Vec<u8>, WriteReport)> {
    if rewrites.is_empty() {
        return Ok((
            original_bytes.to_vec(),
            WriteReport {
                input_bytes: original_bytes.len(),
                output_bytes: original_bytes.len(),
                appended_bytes: 0,
                content_objects: Vec::new(),
            },
        ));
    }
    let object_ceiling = original
        .trailer
        .get(b"Size")
        .ok()
        .and_then(|value| value.as_i64().ok())
        .and_then(|size| u32::try_from(size.saturating_sub(1)).ok())
        .unwrap_or(original.max_id)
        .max(original.max_id);
    let pages = original.get_pages().into_values().collect::<Vec<_>>();
    let mut incremental =
        IncrementalDocument::create_from(original_bytes.to_vec(), original.clone());
    incremental.new_document.max_id = object_ceiling;
    let mut content_objects = Vec::new();
    let mut rewritten_pages = BTreeSet::new();

    for rewrite in rewrites {
        if !rewritten_pages.insert(rewrite.page_index) {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                format!(
                    "output contains duplicate rewrites for page {}",
                    rewrite.page_index
                ),
            ));
        }
        let page_id = pages.get(rewrite.page_index).copied().ok_or_else(|| {
            MimusError::internal(
                InternalReason::OutputBuild,
                format!(
                    "output rewrite references missing page {}",
                    rewrite.page_index
                ),
            )
        })?;
        incremental
            .opt_clone_object_to_new_document(page_id)
            .map_err(output_build_error)?;

        if !rewrite.embedded_fonts.is_empty() {
            install_page_fonts(
                original,
                &mut incremental.new_document,
                page_id,
                &rewrite.embedded_fonts,
                object_ceiling,
            )?;
        }
        let source_content_ids = original.get_page_contents(page_id);
        if source_content_ids.is_empty() {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                format!(
                    "output rewrite references page {} with no content streams",
                    rewrite.page_index
                ),
            ));
        }
        if let Some(replacement) = rewrite
            .replacements
            .iter()
            .find(|replacement| !source_content_ids.contains(&replacement.content_object))
        {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                format!(
                    "page {} replacement references content object {} outside that page",
                    rewrite.page_index, replacement.content_object.0
                ),
            ));
        }

        let mut page_content_objects = Vec::with_capacity(source_content_ids.len());
        for source_id in source_content_ids {
            let source = original
                .get_object(source_id)
                .and_then(Object::as_stream)
                .map_err(output_build_error)?;
            let decoded = pdf_stream::decode(original, source, MAX_STREAM_BYTES)
                .map_err(output_build_error)?;
            let content = apply_span_replacements(
                &decoded,
                rewrite
                    .replacements
                    .iter()
                    .filter(|replacement| replacement.content_object == source_id),
            )?;
            let mut dictionary = source.dict.clone();
            dictionary.remove(b"Length");
            dictionary.remove(b"Filter");
            dictionary.remove(b"DecodeParms");
            let content_id = incremental
                .new_document
                .add_object(Stream::new(dictionary, content).with_compression(false));
            if content_id.0 <= object_ceiling {
                return Err(MimusError::internal(
                    InternalReason::OutputBuild,
                    format!(
                        "incremental object {} did not exceed input ceiling {object_ceiling}",
                        content_id.0
                    ),
                ));
            }
            page_content_objects.push(content_id);
            content_objects.push(content_id);
        }

        let contents = if let [content_id] = page_content_objects.as_slice() {
            Object::Reference(*content_id)
        } else {
            Object::Array(
                page_content_objects
                    .iter()
                    .copied()
                    .map(Object::Reference)
                    .collect(),
            )
        };
        incremental
            .new_document
            .get_object_mut(page_id)
            .and_then(Object::as_dict_mut)
            .map_err(output_build_error)?
            .set("Contents", contents);
    }

    let mut output = Vec::new();
    incremental
        .save_to(&mut output)
        .map_err(output_build_error)?;
    // CONTEXT #36: 完整输入必须是增量输出的字节前缀。该不变量必须在原子发布前
    // 验证，否则失败时已经覆盖目标文件，错误返回也无法挽回半成品。
    if !output.starts_with(original_bytes) {
        return Err(MimusError::internal(
            InternalReason::OutputBuild,
            "incremental output does not preserve the complete input byte prefix",
        ));
    }
    let report = WriteReport {
        input_bytes: original_bytes.len(),
        output_bytes: output.len(),
        appended_bytes: output.len() - original_bytes.len(),
        content_objects,
    };
    Ok((output, report))
}

fn install_page_fonts(
    original: &Document,
    output: &mut Document,
    page_id: ObjectId,
    embedded_fonts: &[EmbeddedFont],
    object_ceiling: u32,
) -> Result<()> {
    let (inline, inherited_ids) = original
        .get_page_resources(page_id)
        .map_err(output_build_error)?;
    let mut resources = if let Some(resources) = inline {
        resources.clone()
    } else if let Some(resource_id) = inherited_ids.first() {
        original
            .get_dictionary(*resource_id)
            .map_err(output_build_error)?
            .clone()
    } else {
        Dictionary::new()
    };
    let mut fonts = match resources.get(b"Font") {
        Ok(Object::Dictionary(fonts)) => fonts.clone(),
        Ok(Object::Reference(fonts_id)) => original
            .get_dictionary(*fonts_id)
            .map_err(output_build_error)?
            .clone(),
        Err(_) => Dictionary::new(),
        Ok(_) => {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                "page Font resources are neither a dictionary nor a reference",
            ));
        }
    };

    for font in embedded_fonts {
        if fonts.has(font.resource_name.as_bytes()) {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                format!("font resource /{} already exists", font.resource_name),
            ));
        }
        let font_id = append_embedded_font(output, font, object_ceiling)?;
        fonts.set(font.resource_name.as_bytes(), Object::Reference(font_id));
    }
    resources.set("Font", Object::Dictionary(fonts));
    let resources_id = output.add_object(resources);
    ensure_appended(resources_id, object_ceiling)?;
    output
        .get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .map_err(output_build_error)?
        .set("Resources", Object::Reference(resources_id));
    Ok(())
}

fn append_embedded_font(
    output: &mut Document,
    font: &EmbeddedFont,
    object_ceiling: u32,
) -> Result<ObjectId> {
    let scale = |value: i16| i64::from(value) * 1000 / i64::from(font.units_per_em);
    let font_file_id = output.add_object(Stream::new(
        dictionary! { "Length1" => font.font_bytes.len() as i64 },
        font.font_bytes.clone(),
    ));
    ensure_appended(font_file_id, object_ceiling)?;
    let descriptor_id = output.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => Object::Name(font.base_font.as_bytes().to_vec()),
        "Flags" => 4,
        "FontBBox" => vec![Object::Integer(-1000), Object::Integer(scale(font.descent)), Object::Integer(2000), Object::Integer(scale(font.ascent))],
        "ItalicAngle" => 0,
        "Ascent" => scale(font.ascent),
        "Descent" => scale(font.descent),
        "CapHeight" => scale(font.cap_height),
        "StemV" => 80,
        "FontFile2" => Object::Reference(font_file_id),
    });
    ensure_appended(descriptor_id, object_ceiling)?;

    let widths = font
        .glyphs
        .iter()
        .flat_map(|(cid, _, advance)| {
            let width = i64::from(*advance) * 1000 / i64::from(font.units_per_em);
            [
                Object::Integer(i64::from(*cid)),
                Object::Array(vec![Object::Integer(width)]),
            ]
        })
        .collect::<Vec<_>>();
    let descendant_id = output.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => Object::Name(font.base_font.as_bytes().to_vec()),
        "CIDSystemInfo" => dictionary! { "Registry" => Object::string_literal("Adobe"), "Ordering" => Object::string_literal("Identity"), "Supplement" => 0 },
        "FontDescriptor" => Object::Reference(descriptor_id),
        "DW" => 1000,
        "W" => Object::Array(widths),
        "CIDToGIDMap" => "Identity",
    });
    ensure_appended(descendant_id, object_ceiling)?;
    let cmap_id = output.add_object(Stream::new(Dictionary::new(), to_unicode_cmap(font)));
    ensure_appended(cmap_id, object_ceiling)?;
    let type0_id = output.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => Object::Name(font.base_font.as_bytes().to_vec()),
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(descendant_id)],
        "ToUnicode" => Object::Reference(cmap_id),
    });
    ensure_appended(type0_id, object_ceiling)?;
    Ok(type0_id)
}

fn to_unicode_cmap(font: &EmbeddedFont) -> Vec<u8> {
    let mut output = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Mimus-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    output.push_str(&format!("{} beginbfchar\n", font.glyphs.len()));
    for (cid, unicode, _) in &font.glyphs {
        let mut utf16 = [0u16; 2];
        let encoded = unicode.encode_utf16(&mut utf16);
        let unicode_hex = encoded
            .iter()
            .map(|value| format!("{value:04X}"))
            .collect::<String>();
        output.push_str(&format!("<{cid:04X}> <{unicode_hex}>\n"));
    }
    output
        .push_str("endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    output.into_bytes()
}

fn ensure_appended(object_id: ObjectId, object_ceiling: u32) -> Result<()> {
    if object_id.0 <= object_ceiling {
        return Err(MimusError::internal(
            InternalReason::OutputBuild,
            format!(
                "incremental object {} did not exceed input ceiling {object_ceiling}",
                object_id.0
            ),
        ));
    }
    Ok(())
}

fn apply_span_replacements<'a>(
    source: &[u8],
    replacements: impl Iterator<Item = &'a ContentSpanReplacement>,
) -> Result<Vec<u8>> {
    let mut replacements = replacements.collect::<Vec<_>>();
    replacements.sort_by_key(|replacement| (replacement.byte_start, replacement.byte_end));

    let mut output = Vec::with_capacity(source.len());
    let mut cursor = 0usize;
    for replacement in replacements {
        if replacement.byte_start > replacement.byte_end || replacement.byte_end > source.len() {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                format!(
                    "content object {} replacement {}..{} is outside decoded length {}",
                    replacement.content_object.0,
                    replacement.byte_start,
                    replacement.byte_end,
                    source.len()
                ),
            ));
        }
        if replacement.byte_start < cursor {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                format!(
                    "content object {} has overlapping replacement at {}..{}",
                    replacement.content_object.0, replacement.byte_start, replacement.byte_end
                ),
            ));
        }
        output.extend_from_slice(&source[cursor..replacement.byte_start]);
        output.extend_from_slice(&replacement.replacement);
        cursor = replacement.byte_end;
    }
    output.extend_from_slice(&source[cursor..]);
    Ok(output)
}

pub(crate) fn publish(output: &Path, bytes: &[u8]) -> Result<()> {
    atomic_publish(output, |file| {
        file.write_all(bytes).map_err(output_write_error)
    })
}

fn atomic_publish(output: &Path, write_output: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    let parent = output
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(MimusError::io(
            IoReason::OutputWrite,
            format!("output directory does not exist: {}", parent.display()),
        ));
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".mimus-")
        .suffix(".pdf.tmp")
        .tempfile_in(parent)
        .map_err(|error| {
            MimusError::io(
                IoReason::OutputWrite,
                format!("could not create an output temporary file: {error}"),
            )
        })?;
    write_output(temporary.as_file_mut())?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(output_write_error)?;
    temporary.as_file().sync_all().map_err(output_write_error)?;
    temporary.persist(output).map_err(|error| {
        MimusError::io(
            IoReason::AtomicPublish,
            format!(
                "could not atomically publish {}: {}",
                output.display(),
                error.error
            ),
        )
    })?;
    Ok(())
}

fn output_build_error(error: impl std::fmt::Display) -> MimusError {
    MimusError::internal(
        InternalReason::OutputBuild,
        format!("could not build output PDF: {error}"),
    )
}

fn output_write_error(error: impl std::fmt::Display) -> MimusError {
    MimusError::io(
        IoReason::OutputWrite,
        format!("could not write output PDF: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf")
    }

    fn multiple_contents_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../corpus/fixtures/unit-parse-05-contents-array-string-parent/unit-parse-05-contents-array-string-parent.pdf",
        )
    }

    fn fixture_path(id: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures")
            .join(id)
            .join(format!("{id}.pdf"))
    }

    fn rewrite() -> PageRewrite {
        PageRewrite {
            page_index: 0,
            replacements: vec![ContentSpanReplacement {
                content_object: (9, 0),
                byte_start: 31,
                byte_end: 38,
                replacement: b"(MIMUS)".to_vec(),
            }],
            reused_fonts: vec![FontRef {
                resource_name: "F1".to_owned(),
                object_number: 5,
                generation: 0,
            }],
            embedded_fonts: Vec::new(),
            typeset_characters: Vec::new(),
        }
    }

    #[test]
    fn writer_appends_a_new_content_object_and_keeps_the_input_prefix() {
        let input = std::fs::read(fixture()).unwrap();
        let document = Document::load_mem(&input).unwrap();
        let original_resources = document.get_object((4, 0)).unwrap().clone();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("roundtrip.pdf");
        let (bytes, report) = build_incremental(&input, &document, &[rewrite()]).unwrap();
        publish(&output, &bytes).unwrap();
        let bytes = std::fs::read(&output).unwrap();
        assert!(bytes.starts_with(&input));
        assert!(report.appended_bytes > 0);
        assert!(report.content_objects[0].0 > document.max_id);
        let reloaded = Document::load(&output).unwrap();
        assert_eq!(reloaded.get_object((4, 0)).unwrap(), &original_resources);
        let page = reloaded.get_pages()[&1];
        assert_eq!(
            reloaded
                .get_object(page)
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"Resources")
                .unwrap()
                .as_reference()
                .unwrap(),
            (4, 0)
        );
    }

    #[test]
    fn empty_rewrite_set_returns_the_exact_input_without_an_increment() {
        let input = std::fs::read(fixture()).unwrap();
        let document = Document::load_mem(&input).unwrap();

        let (bytes, report) = build_incremental(&input, &document, &[]).unwrap();

        assert_eq!(bytes, input);
        assert_eq!(report.input_bytes, report.output_bytes);
        assert_eq!(report.appended_bytes, 0);
        assert!(report.content_objects.is_empty());
    }

    #[test]
    fn span_replacement_preserves_every_byte_outside_the_operand() {
        let replacement = ContentSpanReplacement {
            content_object: (9, 0),
            byte_start: 7,
            byte_end: 12,
            replacement: b"(new)".to_vec(),
        };

        let output =
            apply_span_replacements(b"q 1 0 0(old) Tj Q", std::iter::once(&replacement)).unwrap();

        assert_eq!(output, b"q 1 0 0(new) Tj Q");
    }

    #[test]
    fn overlapping_span_replacements_are_rejected() {
        let replacements = [
            ContentSpanReplacement {
                content_object: (9, 0),
                byte_start: 1,
                byte_end: 4,
                replacement: Vec::new(),
            },
            ContentSpanReplacement {
                content_object: (9, 0),
                byte_start: 3,
                byte_end: 5,
                replacement: Vec::new(),
            },
        ];

        assert!(apply_span_replacements(b"abcdef", replacements.iter()).is_err());
    }

    #[test]
    fn writer_preserves_multiple_contents_as_separate_ordered_streams() {
        let input = std::fs::read(multiple_contents_fixture()).unwrap();
        let document = Document::load_mem(&input).unwrap();
        let page_id = document.get_pages()[&1];
        let source_ids = document.get_page_contents(page_id);
        assert_eq!(source_ids.len(), 2);
        let source_contents = source_ids
            .iter()
            .map(|object_id| {
                document
                    .get_object(*object_id)
                    .unwrap()
                    .as_stream()
                    .unwrap()
                    .decompressed_content()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let rewrite = PageRewrite {
            page_index: 0,
            replacements: vec![ContentSpanReplacement {
                content_object: source_ids[0],
                byte_start: 0,
                byte_end: 1,
                replacement: source_contents[0][..1].to_vec(),
            }],
            reused_fonts: Vec::new(),
            embedded_fonts: Vec::new(),
            typeset_characters: Vec::new(),
        };

        let (output, report) = build_incremental(&input, &document, &[rewrite]).unwrap();
        let reloaded = Document::load_mem(&output).unwrap();
        let output_page_id = reloaded.get_pages()[&1];
        let output_page = reloaded
            .get_object(output_page_id)
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(
            output_page
                .get(b"Contents")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let output_ids = reloaded.get_page_contents(output_page_id);
        let output_contents = output_ids
            .iter()
            .map(|object_id| {
                reloaded
                    .get_object(*object_id)
                    .unwrap()
                    .as_stream()
                    .unwrap()
                    .decompressed_content()
                    .unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(output_contents, source_contents);
        assert_eq!(report.content_objects, output_ids);
        assert!(
            report
                .content_objects
                .iter()
                .all(|object_id| object_id.0 > document.max_id)
        );
    }

    #[test]
    fn writer_preserves_page_boxes_and_raw_rotation_objects() {
        for id in [
            "unit-geom-05-nonzero-origin-boxes",
            "unit-geom-01-rotate-neg90",
        ] {
            let input = std::fs::read(fixture_path(id)).unwrap();
            let document = Document::load_mem(&input).unwrap();
            let input_page_id = document.get_pages()[&1];
            let input_page = document
                .get_object(input_page_id)
                .unwrap()
                .as_dict()
                .unwrap();
            let expected = [b"MediaBox".as_slice(), b"CropBox", b"Rotate"]
                .into_iter()
                .filter_map(|key| {
                    input_page
                        .get(key)
                        .ok()
                        .map(|value| (key.to_vec(), value.clone()))
                })
                .collect::<Vec<_>>();
            let rewrite = PageRewrite {
                page_index: 0,
                replacements: Vec::new(),
                reused_fonts: Vec::new(),
                embedded_fonts: Vec::new(),
                typeset_characters: Vec::new(),
            };

            let (output, _) = build_incremental(&input, &document, &[rewrite]).unwrap();
            let output = Document::load_mem(&output).unwrap();
            let output_page_id = output.get_pages()[&1];
            let output_page = output
                .get_object(output_page_id)
                .unwrap()
                .as_dict()
                .unwrap();

            for (key, value) in expected {
                assert_eq!(output_page.get(&key).unwrap(), &value, "{id} /{key:?}");
            }
        }
    }

    #[test]
    fn failed_temporary_write_preserves_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("existing.pdf");
        std::fs::write(&output, b"existing").unwrap();
        let result = atomic_publish(&output, |file| {
            file.write_all(b"partial").unwrap();
            Err(MimusError::io(IoReason::OutputWrite, "injected failure"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"existing");
    }
}
