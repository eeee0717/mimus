use std::fs::File;
use std::io::Write;
use std::path::Path;

use lopdf::{Dictionary, Document, IncrementalDocument, Object, ObjectId, Stream};

use crate::error::{InternalReason, IoReason, MimusError, Result};
use crate::il::FontRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageRewrite {
    pub page_index: usize,
    pub content: Vec<u8>,
    pub reused_fonts: Vec<FontRef>,
    pub needs_new_font: bool,
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
    let mut content_objects = Vec::with_capacity(rewrites.len());

    for rewrite in rewrites {
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
        let content_id = incremental
            .new_document
            .add_object(Stream::new(Dictionary::new(), rewrite.content.clone()));
        if content_id.0 <= object_ceiling {
            return Err(MimusError::internal(
                InternalReason::OutputBuild,
                format!(
                    "incremental object {} did not exceed input ceiling {object_ceiling}",
                    content_id.0
                ),
            ));
        }
        incremental
            .new_document
            .get_object_mut(page_id)
            .and_then(Object::as_dict_mut)
            .map_err(output_build_error)?
            .set("Contents", content_id);
        content_objects.push(content_id);
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

    fn rewrite() -> PageRewrite {
        PageRewrite {
            page_index: 0,
            content: b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n".to_vec(),
            reused_fonts: vec![FontRef {
                resource_name: "F1".to_owned(),
                object_number: 5,
                generation: 0,
            }],
            needs_new_font: false,
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
