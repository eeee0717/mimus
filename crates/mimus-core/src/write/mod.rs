use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;
use std::path::Path;

use lopdf::{Dictionary, Document, IncrementalDocument, Object, ObjectId, Stream, dictionary};

use crate::error::{InternalReason, IoReason, MimusError, Result};
use crate::il::{FontRef, Point, Rect};
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
    pub typeset_ink_bounds: Vec<Rect>,
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
    pub stripped_link_border_count: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WriteOptions {
    pub strip_link_borders: bool,
    pub bilingual: bool,
}

#[cfg(test)]
pub(crate) fn build_incremental(
    original_bytes: &[u8],
    original: &Document,
    rewrites: &[PageRewrite],
) -> Result<(Vec<u8>, WriteReport)> {
    build_incremental_with_options(original_bytes, original, rewrites, WriteOptions::default())
}

pub(crate) fn build_incremental_with_options(
    original_bytes: &[u8],
    original: &Document,
    rewrites: &[PageRewrite],
    options: WriteOptions,
) -> Result<(Vec<u8>, WriteReport)> {
    if rewrites.is_empty() && !options.strip_link_borders && !options.bilingual {
        return Ok((
            original_bytes.to_vec(),
            WriteReport {
                input_bytes: original_bytes.len(),
                output_bytes: original_bytes.len(),
                appended_bytes: 0,
                content_objects: Vec::new(),
                stripped_link_border_count: 0,
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
    let translated_pages = if options.bilingual {
        let translated_pages = append_translated_pages(
            original,
            &mut incremental.new_document,
            &pages,
            object_ceiling,
        )?;
        interleave_page_tree(original, &mut incremental, &translated_pages, pages.len())?;
        Some(translated_pages)
    } else {
        None
    };
    let stripped_link_border_count = if options.strip_link_borders {
        strip_link_annotation_borders(original, &mut incremental)?
    } else {
        0
    };

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
        let output_page_id = translated_pages
            .as_ref()
            .and_then(|translated| translated.get(&page_id))
            .copied()
            .unwrap_or(page_id);
        if !options.bilingual {
            incremental
                .opt_clone_object_to_new_document(output_page_id)
                .map_err(output_build_error)?;
        }

        if !rewrite.embedded_fonts.is_empty() {
            install_page_fonts(
                original,
                &mut incremental.new_document,
                page_id,
                output_page_id,
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
            .get_object_mut(output_page_id)
            .and_then(Object::as_dict_mut)
            .map_err(output_build_error)?
            .set("Contents", contents);
    }

    if let Some(translated_pages) = translated_pages.as_ref() {
        remap_bilingual_navigation(original, &mut incremental, translated_pages, pages.len())?;
        duplicate_page_labels(original, &mut incremental, pages.len())?;
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
        stripped_link_border_count,
    };
    Ok((output, report))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AnnotationTarget {
    Object(ObjectId),
    PageArray { page_id: ObjectId, index: usize },
    IndirectArray { array_id: ObjectId, index: usize },
}

fn strip_link_annotation_borders(
    original: &Document,
    incremental: &mut IncrementalDocument,
) -> Result<usize> {
    let mut targets = BTreeSet::new();
    for page_id in original.get_pages().into_values() {
        let Ok(page) = original.get_dictionary(page_id) else {
            continue;
        };
        match page.get(b"Annots") {
            Ok(Object::Array(annotations)) => collect_annotation_targets(
                original,
                annotations,
                |index| AnnotationTarget::PageArray { page_id, index },
                &mut targets,
            ),
            Ok(Object::Reference(array_id)) => {
                let Ok(annotations) = original.get_object(*array_id).and_then(Object::as_array)
                else {
                    continue;
                };
                collect_annotation_targets(
                    original,
                    annotations,
                    |index| AnnotationTarget::IndirectArray {
                        array_id: *array_id,
                        index,
                    },
                    &mut targets,
                );
            }
            _ => {}
        }
    }

    for target in &targets {
        match *target {
            AnnotationTarget::Object(object_id) => {
                incremental
                    .opt_clone_object_to_new_document(object_id)
                    .map_err(output_build_error)?;
                let annotation = incremental
                    .new_document
                    .get_object_mut(object_id)
                    .and_then(Object::as_dict_mut)
                    .map_err(output_build_error)?;
                strip_link_border_dictionary(annotation);
            }
            AnnotationTarget::PageArray { page_id, index } => {
                incremental
                    .opt_clone_object_to_new_document(page_id)
                    .map_err(output_build_error)?;
                let annotation = incremental
                    .new_document
                    .get_object_mut(page_id)
                    .and_then(Object::as_dict_mut)
                    .and_then(|page| page.get_mut(b"Annots"))
                    .and_then(Object::as_array_mut)
                    .and_then(|annotations| {
                        annotations
                            .get_mut(index)
                            .ok_or(lopdf::Error::ObjectNotFound(page_id))
                    })
                    .and_then(Object::as_dict_mut)
                    .map_err(output_build_error)?;
                strip_link_border_dictionary(annotation);
            }
            AnnotationTarget::IndirectArray { array_id, index } => {
                incremental
                    .opt_clone_object_to_new_document(array_id)
                    .map_err(output_build_error)?;
                let annotation = incremental
                    .new_document
                    .get_object_mut(array_id)
                    .and_then(Object::as_array_mut)
                    .and_then(|annotations| {
                        annotations
                            .get_mut(index)
                            .ok_or(lopdf::Error::ObjectNotFound(array_id))
                    })
                    .and_then(Object::as_dict_mut)
                    .map_err(output_build_error)?;
                strip_link_border_dictionary(annotation);
            }
        }
    }
    Ok(targets.len())
}

fn append_translated_pages(
    original: &Document,
    output: &mut Document,
    pages: &[ObjectId],
    object_ceiling: u32,
) -> Result<BTreeMap<ObjectId, ObjectId>> {
    let mut translated = BTreeMap::new();
    for page_id in pages {
        let mut page = original
            .get_dictionary(*page_id)
            .map_err(output_build_error)?
            .clone();
        page.remove(b"Annots");
        let translated_id = output.add_object(page);
        ensure_appended(translated_id, object_ceiling)?;
        translated.insert(*page_id, translated_id);
    }
    Ok(translated)
}

fn interleave_page_tree(
    original: &Document,
    incremental: &mut IncrementalDocument,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
) -> Result<()> {
    let root_pages_id = original
        .catalog()
        .and_then(|catalog| catalog.get(b"Pages"))
        .and_then(Object::as_reference)
        .map_err(output_build_error)?;
    let mut active = BTreeSet::new();
    let count = interleave_page_tree_node(
        original,
        incremental,
        root_pages_id,
        translated_pages,
        &mut active,
        0,
    )?;
    if count != source_page_count.saturating_mul(2) {
        return Err(MimusError::internal(
            InternalReason::OutputBuild,
            format!(
                "bilingual page tree contains {count} pages; expected {}",
                source_page_count.saturating_mul(2)
            ),
        ));
    }
    Ok(())
}

fn interleave_page_tree_node(
    original: &Document,
    incremental: &mut IncrementalDocument,
    pages_id: ObjectId,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    active: &mut BTreeSet<ObjectId>,
    depth: usize,
) -> Result<usize> {
    if depth >= 128 || !active.insert(pages_id) {
        return Err(MimusError::internal(
            InternalReason::OutputBuild,
            "bilingual output encountered an invalid page-tree cycle or depth",
        ));
    }
    let original_pages = original
        .get_dictionary(pages_id)
        .map_err(output_build_error)?;
    let kids = original_pages
        .get(b"Kids")
        .and_then(Object::as_array)
        .map_err(output_build_error)?;
    let mut output_kids = Vec::with_capacity(kids.len().saturating_mul(2));
    let mut count = 0usize;
    for kid in kids {
        let kid_id = kid.as_reference().map_err(output_build_error)?;
        output_kids.push(Object::Reference(kid_id));
        if let Some(translated_id) = translated_pages.get(&kid_id) {
            output_kids.push(Object::Reference(*translated_id));
            count += 2;
        } else {
            count += interleave_page_tree_node(
                original,
                incremental,
                kid_id,
                translated_pages,
                active,
                depth + 1,
            )?;
        }
    }
    active.remove(&pages_id);
    incremental
        .opt_clone_object_to_new_document(pages_id)
        .map_err(output_build_error)?;
    let pages = incremental
        .new_document
        .get_object_mut(pages_id)
        .and_then(Object::as_dict_mut)
        .map_err(output_build_error)?;
    pages.set("Kids", Object::Array(output_kids));
    pages.set("Count", i64::try_from(count).map_err(output_build_error)?);
    Ok(count)
}

fn remap_bilingual_navigation(
    original: &Document,
    incremental: &mut IncrementalDocument,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
) -> Result<()> {
    let catalog_id = original
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(output_build_error)?;
    let mut destination_refs = BTreeSet::new();
    let catalog_object = effective_object(original, incremental, catalog_id)?;
    let mut catalog = catalog_object
        .as_dict()
        .map_err(output_build_error)?
        .clone();
    let mut catalog_changed = false;

    if let Ok(outlines_id) = catalog.get(b"Outlines").and_then(Object::as_reference) {
        remap_outline_tree(
            original,
            incremental,
            outlines_id,
            translated_pages,
            source_page_count,
            &mut destination_refs,
        )?;
    }
    if let Ok(destinations) = catalog.get_mut(b"Dests") {
        catalog_changed |= remap_old_destinations(
            original,
            incremental,
            destinations,
            translated_pages,
            source_page_count,
            &mut destination_refs,
        )?;
    }
    if let Ok(names) = catalog.get_mut(b"Names") {
        catalog_changed |= remap_names_dictionary(
            original,
            incremental,
            names,
            translated_pages,
            source_page_count,
            &mut destination_refs,
        )?;
    }
    if catalog_changed {
        incremental
            .new_document
            .set_object(catalog_id, Object::Dictionary(catalog));
    }

    for page_id in original.get_pages().into_values() {
        let page = original
            .get_dictionary(page_id)
            .map_err(output_build_error)?;
        let Ok(annotations) = page.get(b"Annots") else {
            continue;
        };
        remap_page_annotations(
            original,
            incremental,
            page_id,
            annotations,
            translated_pages,
            source_page_count,
            &mut destination_refs,
        )?;
    }
    Ok(())
}

fn effective_object(
    original: &Document,
    incremental: &IncrementalDocument,
    object_id: ObjectId,
) -> Result<Object> {
    incremental
        .new_document
        .get_object(object_id)
        .or_else(|_| original.get_object(object_id))
        .cloned()
        .map_err(output_build_error)
}

fn remap_outline_tree(
    original: &Document,
    incremental: &mut IncrementalDocument,
    outlines_id: ObjectId,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<()> {
    let outlines = original
        .get_dictionary(outlines_id)
        .map_err(output_build_error)?;
    let Ok(first) = outlines.get(b"First").and_then(Object::as_reference) else {
        return Ok(());
    };
    let mut visited = BTreeSet::new();
    remap_outline_chain(
        original,
        incremental,
        first,
        translated_pages,
        source_page_count,
        destination_refs,
        &mut visited,
    )
}

fn remap_outline_chain(
    original: &Document,
    incremental: &mut IncrementalDocument,
    first_id: ObjectId,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
    visited: &mut BTreeSet<ObjectId>,
) -> Result<()> {
    let mut current = Some(first_id);
    while let Some(object_id) = current {
        if !visited.insert(object_id) {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                "bilingual output encountered an outline cycle",
            ));
        }
        let object = effective_object(original, incremental, object_id)?;
        let mut outline = object.as_dict().map_err(output_build_error)?.clone();
        let next = outline
            .get(b"Next")
            .ok()
            .and_then(|value| value.as_reference().ok());
        let first_child = outline
            .get(b"First")
            .ok()
            .and_then(|value| value.as_reference().ok());
        let mut changed = false;
        if let Ok(destination) = outline.get_mut(b"Dest") {
            changed |= remap_destination(
                original,
                incremental,
                destination,
                translated_pages,
                source_page_count,
                destination_refs,
            )?;
        }
        if let Ok(action) = outline.get_mut(b"A") {
            changed |= remap_local_action(
                original,
                incremental,
                action,
                translated_pages,
                source_page_count,
                destination_refs,
            )?;
        }
        if changed {
            incremental
                .new_document
                .set_object(object_id, Object::Dictionary(outline));
        }
        if let Some(child_id) = first_child {
            remap_outline_chain(
                original,
                incremental,
                child_id,
                translated_pages,
                source_page_count,
                destination_refs,
                visited,
            )?;
        }
        current = next;
    }
    Ok(())
}

fn remap_page_annotations(
    original: &Document,
    incremental: &mut IncrementalDocument,
    page_id: ObjectId,
    annotations: &Object,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<()> {
    match annotations {
        Object::Reference(array_id) => remap_annotation_array_object(
            original,
            incremental,
            *array_id,
            translated_pages,
            source_page_count,
            destination_refs,
        ),
        Object::Array(values) => {
            for value in values {
                match value {
                    Object::Reference(annotation_id) => remap_annotation_object(
                        original,
                        incremental,
                        *annotation_id,
                        translated_pages,
                        source_page_count,
                        destination_refs,
                    )?,
                    Object::Dictionary(dictionary) => {
                        let mut probe = dictionary.clone();
                        if remap_link_annotation(
                            original,
                            incremental,
                            &mut probe,
                            translated_pages,
                            source_page_count,
                            destination_refs,
                        )? {
                            return Err(MimusError::internal(
                                InternalReason::OutputBuild,
                                format!(
                                    "bilingual page object {} contains a direct local Link annotation that cannot be remapped without changing the source page",
                                    page_id.0
                                ),
                            ));
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn remap_annotation_array_object(
    original: &Document,
    incremental: &mut IncrementalDocument,
    array_id: ObjectId,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<()> {
    let object = effective_object(original, incremental, array_id)?;
    match object {
        Object::Reference(next_id) => remap_annotation_array_object(
            original,
            incremental,
            next_id,
            translated_pages,
            source_page_count,
            destination_refs,
        ),
        Object::Array(mut values) => {
            let mut changed = false;
            for value in &mut values {
                match value {
                    Object::Reference(annotation_id) => remap_annotation_object(
                        original,
                        incremental,
                        *annotation_id,
                        translated_pages,
                        source_page_count,
                        destination_refs,
                    )?,
                    Object::Dictionary(dictionary) => {
                        changed |= remap_link_annotation(
                            original,
                            incremental,
                            dictionary,
                            translated_pages,
                            source_page_count,
                            destination_refs,
                        )?;
                    }
                    _ => {}
                }
            }
            if changed {
                incremental
                    .new_document
                    .set_object(array_id, Object::Array(values));
            }
            Ok(())
        }
        _ => Err(MimusError::internal(
            InternalReason::OutputBuild,
            "page Annots reference is not an annotation array",
        )),
    }
}

fn remap_annotation_object(
    original: &Document,
    incremental: &mut IncrementalDocument,
    annotation_id: ObjectId,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<()> {
    let object = effective_object(original, incremental, annotation_id)?;
    let mut annotation = object.as_dict().map_err(output_build_error)?.clone();
    if remap_link_annotation(
        original,
        incremental,
        &mut annotation,
        translated_pages,
        source_page_count,
        destination_refs,
    )? {
        incremental
            .new_document
            .set_object(annotation_id, Object::Dictionary(annotation));
    }
    Ok(())
}

fn remap_link_annotation(
    original: &Document,
    incremental: &mut IncrementalDocument,
    annotation: &mut Dictionary,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<bool> {
    if annotation
        .get(b"Subtype")
        .and_then(|value| original.dereference(value).map(|(_, value)| value))
        .and_then(Object::as_name)
        .ok()
        != Some(b"Link".as_slice())
    {
        return Ok(false);
    }
    let mut changed = false;
    if let Ok(destination) = annotation.get_mut(b"Dest") {
        changed |= remap_destination(
            original,
            incremental,
            destination,
            translated_pages,
            source_page_count,
            destination_refs,
        )?;
    }
    if let Ok(action) = annotation.get_mut(b"A") {
        changed |= remap_local_action(
            original,
            incremental,
            action,
            translated_pages,
            source_page_count,
            destination_refs,
        )?;
    }
    Ok(changed)
}

fn remap_local_action(
    original: &Document,
    incremental: &mut IncrementalDocument,
    action: &mut Object,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<bool> {
    match action {
        Object::Reference(action_id) => {
            let object = effective_object(original, incremental, *action_id)?;
            let mut object = object;
            let changed = remap_local_action(
                original,
                incremental,
                &mut object,
                translated_pages,
                source_page_count,
                destination_refs,
            )?;
            if changed {
                incremental.new_document.set_object(*action_id, object);
            }
            Ok(false)
        }
        Object::Dictionary(dictionary) => {
            let is_goto = dictionary
                .get(b"S")
                .and_then(|value| original.dereference(value).map(|(_, value)| value))
                .and_then(Object::as_name)
                .ok()
                == Some(b"GoTo".as_slice());
            if !is_goto {
                return Ok(false);
            }
            let Ok(destination) = dictionary.get_mut(b"D") else {
                return Ok(false);
            };
            remap_destination(
                original,
                incremental,
                destination,
                translated_pages,
                source_page_count,
                destination_refs,
            )
        }
        _ => Ok(false),
    }
}

fn remap_destination(
    original: &Document,
    incremental: &mut IncrementalDocument,
    destination: &mut Object,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<bool> {
    match destination {
        Object::Array(values) => {
            let Some(page) = values.first_mut() else {
                return Ok(false);
            };
            remap_destination_page(page, translated_pages, source_page_count)
        }
        Object::Dictionary(dictionary) => {
            let Ok(value) = dictionary.get_mut(b"D") else {
                return Ok(false);
            };
            remap_destination(
                original,
                incremental,
                value,
                translated_pages,
                source_page_count,
                destination_refs,
            )
        }
        Object::Reference(object_id) if translated_pages.contains_key(object_id) => {
            *object_id = translated_pages[object_id];
            Ok(true)
        }
        Object::Reference(object_id) => {
            if !destination_refs.insert(*object_id) {
                return Ok(false);
            }
            let object = effective_object(original, incremental, *object_id)?;
            let mut object = object;
            let changed = remap_destination(
                original,
                incremental,
                &mut object,
                translated_pages,
                source_page_count,
                destination_refs,
            )?;
            if changed {
                incremental.new_document.set_object(*object_id, object);
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn remap_destination_page(
    page: &mut Object,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
) -> Result<bool> {
    match page {
        Object::Reference(page_id) => {
            let Some(translated_id) = translated_pages.get(page_id) else {
                return Ok(false);
            };
            *page_id = *translated_id;
            Ok(true)
        }
        Object::Integer(page_index) if *page_index >= 0 => {
            let index = usize::try_from(*page_index).map_err(output_build_error)?;
            if index >= source_page_count {
                return Ok(false);
            }
            *page_index = i64::try_from(index.saturating_mul(2).saturating_add(1))
                .map_err(output_build_error)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn remap_old_destinations(
    original: &Document,
    incremental: &mut IncrementalDocument,
    destinations: &mut Object,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<bool> {
    match destinations {
        Object::Reference(object_id) => {
            let object = effective_object(original, incremental, *object_id)?;
            let mut object = object;
            let changed = remap_old_destinations(
                original,
                incremental,
                &mut object,
                translated_pages,
                source_page_count,
                destination_refs,
            )?;
            if changed {
                incremental.new_document.set_object(*object_id, object);
            }
            Ok(false)
        }
        Object::Dictionary(dictionary) => {
            let mut changed = false;
            for (_, destination) in dictionary.iter_mut() {
                changed |= remap_destination(
                    original,
                    incremental,
                    destination,
                    translated_pages,
                    source_page_count,
                    destination_refs,
                )?;
            }
            Ok(changed)
        }
        _ => Ok(false),
    }
}

fn remap_names_dictionary(
    original: &Document,
    incremental: &mut IncrementalDocument,
    names: &mut Object,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
) -> Result<bool> {
    match names {
        Object::Reference(object_id) => {
            let object = effective_object(original, incremental, *object_id)?;
            let mut object = object;
            let changed = remap_names_dictionary(
                original,
                incremental,
                &mut object,
                translated_pages,
                source_page_count,
                destination_refs,
            )?;
            if changed {
                incremental.new_document.set_object(*object_id, object);
            }
            Ok(false)
        }
        Object::Dictionary(dictionary) => {
            let Ok(destinations) = dictionary.get_mut(b"Dests") else {
                return Ok(false);
            };
            remap_name_tree(
                original,
                incremental,
                destinations,
                translated_pages,
                source_page_count,
                destination_refs,
                &mut BTreeSet::new(),
            )
        }
        _ => Ok(false),
    }
}

#[allow(clippy::too_many_arguments)]
fn remap_name_tree(
    original: &Document,
    incremental: &mut IncrementalDocument,
    node: &mut Object,
    translated_pages: &BTreeMap<ObjectId, ObjectId>,
    source_page_count: usize,
    destination_refs: &mut BTreeSet<ObjectId>,
    visited: &mut BTreeSet<ObjectId>,
) -> Result<bool> {
    match node {
        Object::Reference(object_id) => {
            if !visited.insert(*object_id) {
                return Err(MimusError::internal(
                    InternalReason::OutputBuild,
                    "bilingual output encountered a named-destination tree cycle",
                ));
            }
            let object = effective_object(original, incremental, *object_id)?;
            let mut object = object;
            let changed = remap_name_tree(
                original,
                incremental,
                &mut object,
                translated_pages,
                source_page_count,
                destination_refs,
                visited,
            )?;
            if changed {
                incremental.new_document.set_object(*object_id, object);
            }
            Ok(false)
        }
        Object::Dictionary(dictionary) => {
            let mut changed = false;
            if let Ok(names) = dictionary.get_mut(b"Names").and_then(Object::as_array_mut) {
                for destination in names.iter_mut().skip(1).step_by(2) {
                    changed |= remap_destination(
                        original,
                        incremental,
                        destination,
                        translated_pages,
                        source_page_count,
                        destination_refs,
                    )?;
                }
            }
            if let Ok(kids) = dictionary.get_mut(b"Kids").and_then(Object::as_array_mut) {
                for kid in kids {
                    changed |= remap_name_tree(
                        original,
                        incremental,
                        kid,
                        translated_pages,
                        source_page_count,
                        destination_refs,
                        visited,
                    )?;
                }
            }
            Ok(changed)
        }
        _ => Ok(false),
    }
}

fn duplicate_page_labels(
    original: &Document,
    incremental: &mut IncrementalDocument,
    source_page_count: usize,
) -> Result<()> {
    let catalog_id = original
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(output_build_error)?;
    let catalog_object = effective_object(original, incremental, catalog_id)?;
    let mut catalog = catalog_object
        .as_dict()
        .map_err(output_build_error)?
        .clone();
    let Ok(page_labels) = original
        .catalog()
        .and_then(|value| value.get(b"PageLabels"))
    else {
        return Ok(());
    };
    let mut rules = BTreeMap::new();
    collect_page_label_rules(original, page_labels, &mut rules, &mut BTreeSet::new())?;
    if source_page_count > 0 && !rules.contains_key(&0) {
        return Err(MimusError::internal(
            InternalReason::OutputBuild,
            "PageLabels number tree does not define page index 0",
        ));
    }
    let mut nums = Vec::with_capacity(source_page_count.saturating_mul(4));
    for page_index in 0..source_page_count {
        let (range_start, source_rule) =
            rules.range(..=page_index).next_back().ok_or_else(|| {
                MimusError::internal(
                    InternalReason::OutputBuild,
                    "PageLabels number tree has no active rule",
                )
            })?;
        let mut rule = source_rule.clone();
        if rule.has(b"S") {
            let start = rule
                .get(b"St")
                .ok()
                .and_then(|value| value.as_i64().ok())
                .unwrap_or(1);
            rule.set(
                "St",
                start.saturating_add(
                    i64::try_from(page_index - range_start).map_err(output_build_error)?,
                ),
            );
        }
        for output_index in [page_index * 2, page_index * 2 + 1] {
            nums.push(Object::Integer(
                i64::try_from(output_index).map_err(output_build_error)?,
            ));
            nums.push(Object::Dictionary(rule.clone()));
        }
    }
    let duplicated = Object::Dictionary(dictionary! { "Nums" => Object::Array(nums) });
    match page_labels {
        Object::Reference(object_id) => {
            incremental.new_document.set_object(*object_id, duplicated);
        }
        _ => {
            catalog.set("PageLabels", duplicated);
            incremental
                .new_document
                .set_object(catalog_id, Object::Dictionary(catalog));
        }
    }
    Ok(())
}

fn collect_page_label_rules(
    original: &Document,
    node: &Object,
    rules: &mut BTreeMap<usize, Dictionary>,
    visited: &mut BTreeSet<ObjectId>,
) -> Result<()> {
    let dictionary = match node {
        Object::Reference(object_id) => {
            if !visited.insert(*object_id) {
                return Err(MimusError::internal(
                    InternalReason::OutputBuild,
                    "PageLabels number tree contains a cycle",
                ));
            }
            original
                .get_object(*object_id)
                .and_then(Object::as_dict)
                .map_err(output_build_error)?
        }
        Object::Dictionary(dictionary) => dictionary,
        _ => {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                "PageLabels value is not a number-tree dictionary",
            ));
        }
    };
    if let Ok(nums) = dictionary.get(b"Nums").and_then(Object::as_array) {
        if nums.len() % 2 != 0 {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                "PageLabels Nums array has an odd number of entries",
            ));
        }
        for pair in nums.chunks_exact(2) {
            let index = pair[0].as_i64().map_err(output_build_error)?;
            let index = usize::try_from(index).map_err(output_build_error)?;
            let (_, rule) = original.dereference(&pair[1]).map_err(output_build_error)?;
            let rule = rule.as_dict().map_err(output_build_error)?.clone();
            rules.insert(index, rule);
        }
    }
    if let Ok(kids) = dictionary.get(b"Kids").and_then(Object::as_array) {
        for kid in kids {
            collect_page_label_rules(original, kid, rules, visited)?;
        }
    }
    Ok(())
}

fn collect_annotation_targets(
    original: &Document,
    annotations: &[Object],
    direct_target: impl Fn(usize) -> AnnotationTarget,
    targets: &mut BTreeSet<AnnotationTarget>,
) {
    for (index, annotation) in annotations.iter().enumerate() {
        match annotation {
            Object::Reference(object_id) => {
                let Ok(dictionary) = original.get_dictionary(*object_id) else {
                    continue;
                };
                if link_border_is_visible(original, dictionary) {
                    targets.insert(AnnotationTarget::Object(*object_id));
                }
            }
            Object::Dictionary(dictionary) if link_border_is_visible(original, dictionary) => {
                targets.insert(direct_target(index));
            }
            _ => {}
        }
    }
}

fn link_border_is_visible(original: &Document, annotation: &Dictionary) -> bool {
    if annotation
        .get(b"Subtype")
        .and_then(|subtype| original.dereference(subtype).map(|(_, subtype)| subtype))
        .and_then(Object::as_name)
        .ok()
        != Some(b"Link".as_slice())
    {
        return false;
    }
    match annotation
        .get(b"BS")
        .and_then(|style| original.dereference(style).map(|(_, style)| style))
    {
        Ok(Object::Dictionary(style)) => style.get(b"W").ok().is_none_or(|width| {
            original
                .dereference(width)
                .ok()
                .and_then(|(_, width)| width.as_float().ok())
                .is_none_or(|width| width > 0.0)
        }),
        Ok(_) => true,
        Err(_) => annotation
            .get(b"Border")
            .ok()
            .and_then(|border| original.dereference(border).ok())
            .map(|(_, border)| border)
            .and_then(|border| border.as_array().ok())
            .and_then(|border| border.get(2))
            .and_then(|width| original.dereference(width).ok())
            .map(|(_, width)| width)
            .and_then(|width| width.as_float().ok())
            .is_none_or(|width| width > 0.0),
    }
}

fn strip_link_border_dictionary(annotation: &mut Dictionary) {
    annotation.set(
        "Border",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(0),
        ]),
    );
    annotation.remove(b"BS");
}

fn install_page_fonts(
    original: &Document,
    output: &mut Document,
    source_page_id: ObjectId,
    output_page_id: ObjectId,
    embedded_fonts: &[EmbeddedFont],
    object_ceiling: u32,
) -> Result<()> {
    let (inline, inherited_ids) = original
        .get_page_resources(source_page_id)
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
        .get_object_mut(output_page_id)
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
            [
                Object::Integer(i64::from(*cid)),
                Object::Array(vec![Object::Integer(i64::from(glyph_width_1000(
                    *advance,
                    font.units_per_em,
                )))]),
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

/// Normalizes source-font advances to the integer 1000-em widths stored in PDF `/W`.
pub(crate) fn glyph_width_1000(advance: u16, units_per_em: u16) -> u32 {
    let numerator = u64::from(advance) * 1000 + u64::from(units_per_em) / 2;
    let denominator = u64::from(units_per_em);
    u32::try_from(numerator / denominator).expect("font width fits in a PDF width integer")
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

    fn embedded_test_font() -> EmbeddedFont {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mimus/tests/assets/fonts/MimusTestGB2312-Regular.ttf"
        ));
        let face = ttf_parser::Face::parse(bytes, 0).unwrap();
        let glyph = face.glyph_index('M').unwrap();
        EmbeddedFont {
            resource_name: "MimusR".to_owned(),
            base_font: "MIMUSW+NotoSansSC-Regular".to_owned(),
            font_bytes: bytes.to_vec(),
            units_per_em: face.units_per_em(),
            ascent: face.ascender(),
            descent: face.descender(),
            cap_height: face.capital_height().unwrap_or(face.ascender()),
            glyphs: vec![(glyph.0, 'M', face.glyph_hor_advance(glyph).unwrap())],
        }
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
            typeset_ink_bounds: Vec::new(),
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
    fn bilingual_writer_does_not_synthesize_page_labels() {
        let input = std::fs::read(fixture()).unwrap();
        let original = Document::load_mem(&input).unwrap();

        let (bytes, report) = build_incremental_with_options(
            &input,
            &original,
            &[],
            WriteOptions {
                strip_link_borders: false,
                bilingual: true,
            },
        )
        .unwrap();

        assert!(bytes.starts_with(&input));
        assert!(report.appended_bytes > 0);
        let output = Document::load_mem(&bytes).unwrap();
        assert_eq!(output.get_pages().len(), 2);
        assert!(!output.catalog().unwrap().has(b"PageLabels"));
        assert_eq!(
            output.get_object((3, 0)).unwrap(),
            original.get_object((3, 0)).unwrap()
        );
    }

    #[test]
    fn indirect_zero_width_link_styles_are_already_borderless() {
        let mut document = Document::new();
        let width = document.add_object(Object::Integer(0));
        let style = document.add_object(dictionary! {
            "W" => Object::Reference(width),
        });
        let border = document.add_object(Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(0),
        ]));
        let mut annotation = Dictionary::new();
        annotation.set("Subtype", Object::Name(b"Link".to_vec()));
        annotation.set("BS", Object::Reference(style));
        assert!(!link_border_is_visible(&document, &annotation));

        annotation.remove(b"BS");
        annotation.set("Border", Object::Reference(border));
        assert!(!link_border_is_visible(&document, &annotation));
    }

    #[test]
    fn link_border_cleanup_is_opt_in_and_preserves_annotation_semantics() {
        let input = std::fs::read(fixture_path("unit-write-07-link-borders")).unwrap();
        let original = Document::load_mem(&input).unwrap();

        let (default_output, default_report) = build_incremental(&input, &original, &[]).unwrap();
        assert_eq!(default_output, input);
        assert_eq!(default_report.stripped_link_border_count, 0);

        let (output, report) = build_incremental_with_options(
            &input,
            &original,
            &[],
            WriteOptions {
                strip_link_borders: true,
                bilingual: false,
            },
        )
        .unwrap();
        assert!(output.starts_with(&input));
        assert_eq!(report.stripped_link_border_count, 2);
        assert!(report.content_objects.is_empty());

        let output = Document::load_mem(&output).unwrap();
        for object_id in [(10, 0), (11, 0)] {
            let annotation = output.get_dictionary(object_id).unwrap();
            assert_eq!(
                annotation.get(b"Border").unwrap().as_array().unwrap(),
                &vec![Object::Integer(0), Object::Integer(0), Object::Integer(0)]
            );
            assert!(!annotation.has(b"BS"));
        }
        for object_id in [(12, 0), (13, 0)] {
            assert_eq!(
                output.get_object(object_id).unwrap(),
                original.get_object(object_id).unwrap(),
                "control annotation {object_id:?} changed"
            );
        }

        let input_link = original.get_dictionary((10, 0)).unwrap();
        let output_link = output.get_dictionary((10, 0)).unwrap();
        assert_eq!(
            output_link.get(b"Rect").unwrap(),
            input_link.get(b"Rect").unwrap()
        );
        assert_eq!(
            output_link.get(b"A").unwrap(),
            input_link.get(b"A").unwrap()
        );
        let input_link = original.get_dictionary((11, 0)).unwrap();
        let output_link = output.get_dictionary((11, 0)).unwrap();
        assert_eq!(
            output_link.get(b"Rect").unwrap(),
            input_link.get(b"Rect").unwrap()
        );
        assert_eq!(
            output_link.get(b"Dest").unwrap(),
            input_link.get(b"Dest").unwrap()
        );
        assert_eq!(
            output.get_dictionary((13, 0)).unwrap().get(b"AP").unwrap(),
            original
                .get_dictionary((13, 0))
                .unwrap()
                .get(b"AP")
                .unwrap()
        );
    }

    #[test]
    fn bilingual_writer_interleaves_pages_and_remaps_only_local_navigation() {
        let input = std::fs::read(fixture_path("unit-write-08-bilingual-navigation")).unwrap();
        let original = Document::load_mem(&input).unwrap();
        let source_content = original
            .get_object((10, 0))
            .unwrap()
            .as_stream()
            .unwrap()
            .decompressed_content()
            .unwrap();
        let byte_start = source_content
            .windows(b"(MIMUS)".len())
            .position(|window| window == b"(MIMUS)")
            .unwrap();
        let rewrite = PageRewrite {
            page_index: 0,
            replacements: vec![ContentSpanReplacement {
                content_object: (10, 0),
                byte_start,
                byte_end: byte_start + b"(MIMUS)".len(),
                replacement: b"(MIMUS MIMUS)".to_vec(),
            }],
            reused_fonts: Vec::new(),
            embedded_fonts: Vec::new(),
            typeset_characters: Vec::new(),
            typeset_ink_bounds: Vec::new(),
        };

        let (bytes, _) = build_incremental_with_options(
            &input,
            &original,
            &[rewrite],
            WriteOptions {
                strip_link_borders: false,
                bilingual: true,
            },
        )
        .unwrap();
        assert!(bytes.starts_with(&input));
        let output = Document::load_mem(&bytes).unwrap();
        let pages = output.get_pages().into_values().collect::<Vec<_>>();
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0], (3, 0));
        assert_eq!(pages[2], (4, 0));
        assert!(pages[1].0 > original.max_id && pages[3].0 > original.max_id);
        for source_page in [(3, 0), (4, 0)] {
            assert_eq!(
                output.get_object(source_page).unwrap(),
                original.get_object(source_page).unwrap(),
                "source page object {source_page:?} changed"
            );
        }
        for translated_page in [pages[1], pages[3]] {
            let translated = output.get_dictionary(translated_page).unwrap();
            assert!(!translated.has(b"Annots"));
            let source = original
                .get_dictionary(if translated_page == pages[1] {
                    (3, 0)
                } else {
                    (4, 0)
                })
                .unwrap();
            for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate"] {
                assert_eq!(translated.get(key).ok(), source.get(key).ok());
            }
        }
        assert_eq!(
            output
                .get_object(output.get_page_contents(pages[0])[0])
                .unwrap()
                .as_stream()
                .unwrap()
                .decompressed_content()
                .unwrap(),
            source_content
        );
        assert!(
            output
                .get_object(output.get_page_contents(pages[1])[0])
                .unwrap()
                .as_stream()
                .unwrap()
                .decompressed_content()
                .unwrap()
                .windows(b"(MIMUS MIMUS)".len())
                .any(|window| window == b"(MIMUS MIMUS)")
        );

        let root_pages = output.get_dictionary((2, 0)).unwrap();
        assert_eq!(root_pages.get(b"Count").unwrap().as_i64().unwrap(), 4);
        let leaf_pages = output.get_dictionary((20, 0)).unwrap();
        assert_eq!(leaf_pages.get(b"Count").unwrap().as_i64().unwrap(), 4);
        assert_eq!(
            leaf_pages.get(b"Kids").unwrap().as_array().unwrap(),
            &pages
                .iter()
                .copied()
                .map(Object::Reference)
                .collect::<Vec<_>>()
        );

        let outline_exact = output.get_dictionary((13, 0)).unwrap();
        let exact_destination = outline_exact.get(b"Dest").unwrap().as_array().unwrap();
        assert_eq!(exact_destination[0], Object::Reference(pages[3]));
        assert_eq!(
            &exact_destination[1..],
            &[
                Object::Name(b"XYZ".to_vec()),
                Object::Integer(72),
                Object::Integer(120),
                Object::Real(1.25),
            ]
        );
        assert_eq!(
            output
                .get_dictionary((14, 0))
                .unwrap()
                .get(b"Dest")
                .unwrap(),
            original
                .get_dictionary((14, 0))
                .unwrap()
                .get(b"Dest")
                .unwrap()
        );
        let goto = output
            .get_dictionary((15, 0))
            .unwrap()
            .get(b"A")
            .unwrap()
            .as_dict()
            .unwrap();
        let goto_destination = goto.get(b"D").unwrap().as_array().unwrap();
        assert_eq!(goto_destination[0], Object::Reference(pages[1]));
        assert_eq!(
            &goto_destination[1..],
            &[
                Object::Name(b"XYZ".to_vec()),
                Object::Integer(72),
                Object::Integer(144),
                Object::Integer(0),
            ]
        );

        let names = output
            .get_dictionary((16, 0))
            .unwrap()
            .get(b"Names")
            .unwrap()
            .as_array()
            .unwrap();
        let named_destination = names[1].as_array().unwrap();
        assert_eq!(named_destination[0], Object::Reference(pages[3]));
        assert_eq!(
            &named_destination[1..],
            &[
                Object::Name(b"XYZ".to_vec()),
                Object::Integer(72),
                Object::Integer(120),
                Object::Real(1.25),
            ]
        );
        let catalog = output.catalog().unwrap();
        let legacy_destination = catalog
            .get(b"Dests")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"legacy")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(legacy_destination[0], Object::Reference(pages[1]));
        assert_eq!(
            &legacy_destination[1..],
            &[
                Object::Name(b"FitR".to_vec()),
                Object::Integer(10),
                Object::Integer(20),
                Object::Integer(290),
                Object::Integer(180),
            ]
        );

        let link = output.get_dictionary((17, 0)).unwrap();
        let link_destination = link
            .get(b"A")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"D")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(link_destination[0], Object::Reference(pages[3]));
        assert_eq!(
            link.get(b"Rect").unwrap(),
            original
                .get_dictionary((17, 0))
                .unwrap()
                .get(b"Rect")
                .unwrap()
        );
        assert_eq!(
            output.get_object((18, 0)).unwrap(),
            original.get_object((18, 0)).unwrap(),
            "URI annotation changed"
        );
        for key in [b"AcroForm".as_slice(), b"OCProperties"] {
            assert_eq!(
                catalog.get(key).unwrap(),
                original.catalog().unwrap().get(key).unwrap()
            );
        }

        let labels = output
            .get_dictionary((23, 0))
            .unwrap()
            .get(b"Nums")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(labels.len(), 8);
        let starts = labels
            .iter()
            .skip(1)
            .step_by(2)
            .map(|value| {
                value
                    .as_dict()
                    .unwrap()
                    .get(b"St")
                    .unwrap()
                    .as_i64()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(starts, vec![3, 3, 7, 7]);
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
            typeset_ink_bounds: Vec::new(),
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
                typeset_ink_bounds: Vec::new(),
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
    fn embedded_font_copy_on_write_covers_all_resource_shapes() {
        for id in [
            "unit-write-01-bookmarks-rich",
            "unit-write-02-shared-resources",
            "unit-write-03-resources-gen-nonzero",
            "unit-write-04-xobj-in-objstm",
            "unit-write-05-indirect-resources-objstm",
            "unit-write-06-free-object-slot",
        ] {
            let input = std::fs::read(fixture_path(id)).unwrap();
            let original = Document::load_mem(&input).unwrap();
            let page_ids = original.get_pages().into_values().collect::<Vec<_>>();
            let page_id = page_ids[0];
            let original_page = original.get_dictionary(page_id).unwrap().clone();
            let (_, resource_ids) = original.get_page_resources(page_id).unwrap();
            let original_resources = resource_ids
                .iter()
                .map(|object_id| (*object_id, original.get_object(*object_id).unwrap().clone()))
                .collect::<Vec<_>>();
            let content_id = original.get_page_contents(page_id)[0];
            let decoded = original
                .get_object(content_id)
                .unwrap()
                .as_stream()
                .unwrap()
                .decompressed_content()
                .unwrap();
            let rewrite = PageRewrite {
                page_index: 0,
                replacements: vec![ContentSpanReplacement {
                    content_object: content_id,
                    byte_start: 0,
                    byte_end: 1,
                    replacement: decoded[..1].to_vec(),
                }],
                reused_fonts: Vec::new(),
                embedded_fonts: vec![embedded_test_font()],
                typeset_characters: Vec::new(),
                typeset_ink_bounds: Vec::new(),
            };

            let (bytes, report) = build_incremental(&input, &original, &[rewrite]).unwrap();
            assert!(bytes.starts_with(&input), "{id}");
            assert!(
                report
                    .content_objects
                    .iter()
                    .all(|value| value.0 > original.max_id),
                "{id}"
            );
            let output = Document::load_mem(&bytes).unwrap();
            let output_page = output.get_dictionary(page_id).unwrap();
            for (key, value) in original_page.iter() {
                if key != b"Contents" && key != b"Resources" {
                    assert_eq!(
                        output_page.get(key).unwrap(),
                        value,
                        "{id} /{:?}",
                        String::from_utf8_lossy(key)
                    );
                }
            }
            let output_resources = output_page
                .get(b"Resources")
                .unwrap()
                .as_reference()
                .unwrap();
            assert!(output_resources.0 > original.max_id, "{id}");
            assert!(
                output
                    .get_page_fonts(page_id)
                    .unwrap()
                    .contains_key(b"MimusR".as_slice()),
                "{id}"
            );
            for (object_id, expected) in original_resources {
                assert_eq!(
                    output.get_object(object_id).unwrap(),
                    &expected,
                    "{id} resource {object_id:?}"
                );
            }
            if id == "unit-write-02-shared-resources" {
                assert_eq!(
                    output
                        .get_dictionary(page_ids[1])
                        .unwrap()
                        .get(b"Resources")
                        .unwrap(),
                    original
                        .get_dictionary(page_ids[1])
                        .unwrap()
                        .get(b"Resources")
                        .unwrap(),
                );
            }
            if id == "unit-write-06-free-object-slot" {
                assert!(report.content_objects.iter().all(|value| value.0 > 10));
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
