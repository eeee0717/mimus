use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, ensure};
use lopdf::{Document, IncrementalDocument, Object, Stream, dictionary};
use sha2::{Digest, Sha256};

const INPUT: &str = "corpus/fixtures/unit-base-03-structured/unit-base-03-structured.pdf";

fn main() -> Result<()> {
    let root = repo_root()?;
    let input = root.join(INPUT);
    let work = root.join(".context/m0-lab/poc");
    fs::create_dir_all(&work)?;
    let output = work.join("incremental-output.pdf");
    let report = run(&input, &output)?;
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

fn run(input: &Path, output: &Path) -> Result<String> {
    let work = output.parent().context("output has no parent")?;
    let root = repo_root()?;
    verify_companion_inputs(&root)?;
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
    // Append a real Standard-14 font and wire it into the copied resource
    // dictionary. The new content must reference this font; an unreferenced
    // marker would not exercise glyph preservation at all.
    let marker_id = append_font_marker(&mut incremental, false)?;
    {
        let resources = incremental
            .new_document
            .get_object_mut(new_resources)?
            .as_dict_mut()?;
        resources
            .get_mut(b"Font")?
            .as_dict_mut()?
            .set("F2", marker_id);
    }
    {
        let page = incremental
            .new_document
            .get_object_mut(input_page)?
            .as_dict_mut()?;
        page.set("Resources", new_resources);
    }

    let content = b"BT\n/F2 12 Tf\n1 0 0 1 72 120 Tm\n(POC) Tj\nET\n";
    let content_id = incremental.new_document.add_object(Stream::new(
        dictionary! { "Length" => Object::Integer(content.len() as i64) },
        content.to_vec(),
    ));
    incremental
        .new_document
        .get_object_mut(input_page)?
        .as_dict_mut()?
        .set("Contents", content_id);

    let output_bytes = save_incremental(&mut incremental, output, false)?;
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
    ensure!(marker_id.0 != 10 && new_resources.0 != 10 && content_id.0 != 10);
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
    let output_font = reloaded
        .get_object(output_resources)?
        .as_dict()?
        .get(b"Font")?
        .as_dict()?
        .get(b"F2")?
        .as_reference()?;
    ensure!(
        output_font == marker_id,
        "new font is not referenced by COW resources"
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
    let mut failed_incremental = IncrementalDocument::create_from(before.clone(), document);
    let failed_target = work.join("existing-output.pdf");
    fs::write(&failed_target, b"known-good")?;
    let before_failed = sha256(&fs::read(&failed_target)?);
    let failure = save_incremental(&mut failed_incremental, &failed_target, true)
        .expect_err("injected save failure should fail");
    ensure!(sha256(&fs::read(&failed_target)?) == before_failed);
    let temp = work.join(format!(".{}.tmp", std::process::id()));
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

fn verify_companion_inputs(root: &Path) -> Result<()> {
    let objstm_path =
        root.join("corpus/fixtures/unit-write-04-xobj-in-objstm/unit-write-04-xobj-in-objstm.pdf");
    let objstm_bytes = fs::read(&objstm_path)?;
    ensure!(objstm_bytes.windows(13).any(|w| w == b"/Type /ObjStm"));
    let objstm = Document::load(&objstm_path)?;
    let page = objstm
        .get_pages()
        .into_iter()
        .next()
        .context("ObjStm fixture has no page")?
        .1;
    let resources = objstm
        .get_object(page)?
        .as_dict()?
        .get(b"Resources")?
        .as_reference()?;
    let xobject = objstm
        .get_object(resources)?
        .as_dict()?
        .get(b"XObject")?
        .as_dict()?
        .get(b"X1")?
        .as_reference()?;
    let form = objstm.get_object(xobject)?.as_stream()?;
    ensure!(form.content.windows(4).any(|w| w == b"FORM"));

    let geometry_path = root.join(
        "corpus/fixtures/unit-geom-05-nonzero-origin-boxes/unit-geom-05-nonzero-origin-boxes.pdf",
    );
    let geometry = Document::load(&geometry_path)?;
    let geometry_page = geometry
        .get_pages()
        .into_iter()
        .next()
        .context("geometry fixture has no page")?
        .1;
    let geometry_dict = geometry.get_object(geometry_page)?.as_dict()?;
    ensure!(geometry_dict.get(b"MediaBox")?.as_array()?.len() == 4);
    ensure!(geometry_dict.get(b"CropBox")?.as_array()?.len() == 4);

    let generation_path = root.join(
        "corpus/fixtures/unit-write-03-resources-gen-nonzero/unit-write-03-resources-gen-nonzero.pdf",
    );
    let generation = Document::load(&generation_path)?;
    ensure!(generation.objects.contains_key(&(4, 7)));
    let generation_page = generation
        .get_pages()
        .into_iter()
        .next()
        .context("generation fixture has no page")?
        .1;
    ensure!(
        generation
            .get_object(generation_page)?
            .as_dict()?
            .get(b"Resources")?
            .as_reference()?
            == (4, 7)
    );

    let free_path = root
        .join("corpus/fixtures/unit-write-06-free-object-slot/unit-write-06-free-object-slot.pdf");
    let free_bytes = fs::read(&free_path)?;
    ensure!(free_bytes.windows(10).any(|w| w == b"xref\n0 11\n"));
    ensure!(
        free_bytes
            .windows(20)
            .any(|w| w == b"0000000000 00000 f \n")
    );
    let free = Document::load(&free_path)?;
    ensure!(!object_numbers(&free).contains(&10));
    Ok(())
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

fn save_incremental(
    document: &mut IncrementalDocument,
    output: &Path,
    inject_failure: bool,
) -> Result<Vec<u8>> {
    let parent = output.parent().context("output has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", std::process::id()));
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .context("serialize incremental lopdf document")?;
    fs::write(&temp, &bytes)?;
    if inject_failure {
        let _ = fs::remove_file(&temp);
        return Err(anyhow!("injected save failure after temporary write"));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_writeback_keeps_prefix_and_publishes_referenced_font() {
        let root = repo_root().unwrap();
        let work = std::env::temp_dir().join(format!(
            "mimus-m0-experiment-3-poc-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&work).unwrap();
        let input = root.join(INPUT);
        let output = work.join("output.pdf");
        let report = run(&input, &output).unwrap();
        assert!(report.contains("new_font=20"));
        assert!(report.contains("save_failure=injected save failure"));
        std::fs::remove_dir_all(work).unwrap();
    }
}
