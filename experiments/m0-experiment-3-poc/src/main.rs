use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use lopdf::{Document, IncrementalDocument, Object, Stream, dictionary};
use sha2::{Digest, Sha256};

const INPUT: &str = "corpus/fixtures/unit-base-03-structured/unit-base-03-structured.pdf";

fn main() -> Result<()> {
    let root = repo_root()?;
    let input = root.join(INPUT);
    let work = root.join(".context/m0-lab/poc");
    fs::create_dir_all(&work)?;
    let output = work.join("incremental-output.pdf");
    let failed = work.join("failed-output");
    let report = run(&input, &output, &failed)?;
    println!("{report}");
    Ok(())
}

fn repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .find(|path| path.join("corpus/toolchain.toml").is_file())
        .map(Path::to_path_buf)
        .context("cannot locate repository root")
}

fn run(input: &Path, output: &Path, failed: &Path) -> Result<String> {
    let before = fs::read(input)?;
    let before_sha = sha256(&before);
    let document = Document::load(input).context("load input with lopdf")?;
    let mut incremental = IncrementalDocument::create_from(before.clone(), document.clone());
    let input_objects = object_numbers(&document);
    let max_id = *input_objects.iter().max().context("input has no objects")?;
    let input_page = document
        .get_pages()
        .into_iter()
        .next()
        .map(|(_, id)| id)
        .context("input has no page")?;

    // Copy-on-write: clone the shared resources dictionary and point only the
    // target page at the clone. The original resource object remains untouched.
    let resources_id = document
        .get_object(input_page)?
        .as_dict()?
        .get(b"Resources")?
        .as_reference()?;
    incremental.opt_clone_object_to_new_document(input_page)?;
    let resources = document.get_object(resources_id)?.clone();
    let new_resources = copy_resources(&mut incremental, resources, false)?;
    {
        let page = incremental
            .new_document
            .get_object_mut(input_page)?
            .as_dict_mut()?;
        page.set("Resources", new_resources);
    }

    let content_id = incremental.new_document.add_object(Stream::new(
        dictionary! { "Length" => Object::Integer(42) },
        b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(POC) Tj\nET\n".to_vec(),
    ));
    incremental
        .new_document
        .get_object_mut(input_page)?
        .as_dict_mut()?
        .set("Contents", content_id);

    // A tiny appended font marker exercises the same object-allocation path as
    // a real embedded font while keeping this experiment independent of font
    // subsetting and translation code.
    let marker_id = append_font_marker(&mut incremental, false)?;
    let output_bytes = save_incremental(&mut incremental, output)?;
    let output_sha = sha256(&output_bytes);
    ensure!(
        output_bytes.starts_with(&before),
        "output is not input + append"
    );
    ensure!(
        marker_id.0 > max_id,
        "new object number reused an input object"
    );
    ensure!(new_resources.0 > max_id, "COW resource was not appended");
    ensure!(content_id.0 > max_id, "content object is not appended");
    ensure!(
        fs::read(output)? == output_bytes,
        "output changed after save"
    );

    let reloaded = Document::load(output).context("reload incremental output")?;
    ensure!(reloaded.get_pages().len() == document.get_pages().len());
    ensure!(object_numbers(&reloaded).contains(&marker_id.0));
    let input_page_resources = Document::load(input)?.get_object(resources_id)?.clone();
    let output_page = reloaded
        .get_pages()
        .into_iter()
        .next()
        .map(|(_, id)| id)
        .context("output has no page")?;
    let output_resources = reloaded
        .get_object(output_page)?
        .as_dict()?
        .get(b"Resources")?
        .as_reference()?;
    ensure!(
        output_resources == new_resources,
        "target page did not use COW resources"
    );
    ensure!(reloaded.get_object(resources_id)? == &input_page_resources);
    let root = reloaded.trailer.get(b"Root")?.as_reference()?;
    let catalog_resources = reloaded
        .get_object(root)?
        .as_dict()?
        .get(b"AcroForm")?
        .as_dict()?
        .get(b"DR")?
        .as_reference()?;
    ensure!(
        catalog_resources == resources_id,
        "catalog lost original resources"
    );

    // Rich structure objects are never touched by this update.
    for object_id in [1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18] {
        ensure!(
            reloaded.get_object((object_id, 0))? == document.get_object((object_id, 0))?,
            "unmodified object {object_id} changed"
        );
    }
    let original_page = document.get_object(input_page)?.as_dict()?;
    let output_page_dict = reloaded.get_object(output_page)?.as_dict()?;
    for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate"] {
        ensure!(
            original_page.get(key).ok() == output_page_dict.get(key).ok(),
            "page box key {:?} changed",
            String::from_utf8_lossy(key)
        );
    }

    // Failure injection: resource copying and font appending fail before a
    // destination is published, leaving an existing destination untouched.
    let resource_failure = work_sentinel(output, "resource-failure")?;
    let font_failure = work_sentinel(output, "font-failure")?;
    let resource_document = Document::load(input)?;
    let mut resource_incremental =
        IncrementalDocument::create_from(before.clone(), resource_document);
    let resource_error = copy_resources(&mut resource_incremental, Object::Null, true)
        .expect_err("injected resource copy failure");
    ensure!(fs::read(&resource_failure)? == b"known-good");
    let font_document = Document::load(input)?;
    let mut font_incremental = IncrementalDocument::create_from(before.clone(), font_document);
    let font_error =
        append_font_marker(&mut font_incremental, true).expect_err("injected font append failure");
    ensure!(fs::read(&font_failure)? == b"known-good");

    // Failure injection: an unwritable destination must not replace an
    // existing file, and the temporary output must be cleaned up.
    fs::create_dir_all(&failed)?;
    let failed_sentinel = failed.join("sentinel.pdf");
    fs::write(&failed_sentinel, b"known-good")?;
    let blocked_target = failed.join("child");
    fs::create_dir_all(&blocked_target)?;
    let mut failed_incremental = IncrementalDocument::create_from(before.clone(), document);
    let failure = save_incremental(&mut failed_incremental, &blocked_target)
        .expect_err("directory target should fail");
    ensure!(fs::read(&failed_sentinel)? == b"known-good");
    let temp = failed.join(format!(".{}.tmp", std::process::id()));
    ensure!(!temp.exists(), "failed save left a temporary file");
    let failure_text = format!("{failure:#}");

    Ok(format!(
        "input_sha256={before_sha}\noutput_sha256={output_sha}\ninput_len={} output_len={} appended_bytes={} max_input_object={} new_resources={} new_content={} new_font={} resource_failure={resource_error} font_failure={font_error} save_failure={failure_text}",
        before.len(),
        output_bytes.len(),
        output_bytes.len() - before.len(),
        max_id,
        new_resources.0,
        content_id.0,
        marker_id.0,
    ))
}

fn copy_resources(
    incremental: &mut IncrementalDocument,
    resources: Object,
    inject_failure: bool,
) -> Result<lopdf::ObjectId> {
    ensure!(!inject_failure, "injected resource copy failure");
    Ok(incremental.new_document.add_object(resources))
}

fn append_font_marker(
    incremental: &mut IncrementalDocument,
    inject_failure: bool,
) -> Result<lopdf::ObjectId> {
    ensure!(!inject_failure, "injected font append failure");
    Ok(incremental.new_document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    }))
}

fn work_sentinel(output: &Path, name: &str) -> Result<PathBuf> {
    let path = output
        .parent()
        .context("output has no parent")?
        .join(format!("{name}.pdf"));
    fs::write(&path, b"known-good")?;
    Ok(path)
}

fn save_incremental(document: &mut IncrementalDocument, output: &Path) -> Result<Vec<u8>> {
    let parent = output.parent().context("output has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", std::process::id()));
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .context("serialize incremental lopdf document")?;
    fs::write(&temp, &bytes)?;
    if let Err(error) = fs::rename(&temp, output) {
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| format!("rename {}", temp.display()));
    }
    Ok(bytes)
}

fn object_numbers(document: &Document) -> BTreeSet<u32> {
    document.objects.keys().map(|(id, _)| *id).collect()
}

fn sha256(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}
