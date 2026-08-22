use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, ensure};
use lopdf::{Document, IncrementalDocument, Object, ObjectId, Stream, dictionary};
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
    let primary = incremental_rewrite(
        input,
        output,
        0,
        RewriteScenario::ReplaceText { x: 72, y: 120 },
        None,
    )?;
    let before = &primary.before;
    let document = &primary.document;
    let output_bytes = &primary.output_bytes;
    let output_sha = sha256(output_bytes);
    let max_id = primary.max_id;
    ensure!(
        output_bytes.starts_with(before),
        "output is not input + append"
    );
    ensure!(
        primary.font.0 > max_id,
        "new object number reused an input object"
    );
    ensure!(
        primary.resources.0 > max_id,
        "COW resource was not appended"
    );
    ensure!(primary.content.0 > max_id, "content object is not appended");
    ensure!(primary.font.0 != 10 && primary.resources.0 != 10 && primary.content.0 != 10);
    ensure!(
        fs::read(output)? == *output_bytes,
        "output changed after save"
    );
    let reloaded = &primary.output_document;
    ensure!(reloaded.get_pages().len() == document.get_pages().len());
    ensure!(object_numbers(reloaded).contains(&primary.font.0));
    let input_page_resources = document.get_object(primary.original_resources)?.clone();
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
        output_resources == primary.resources,
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
        output_font == primary.font,
        "new font is not referenced by COW resources"
    );
    ensure!(reloaded.get_object(primary.original_resources)? == &input_page_resources);
    let catalog_root = reloaded.trailer.get(b"Root")?.as_reference()?;
    let catalog_resources = reloaded
        .get_object(catalog_root)?
        .as_dict()?
        .get(b"AcroForm")?
        .as_dict()?
        .get(b"DR")?
        .as_reference()?;
    ensure!(
        catalog_resources == primary.original_resources,
        "catalog lost original resources"
    );

    // Rich structure objects are never touched by this update.
    for object_id in [1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18] {
        ensure!(
            reloaded.get_object((object_id, 0))? == document.get_object((object_id, 0))?,
            "unmodified object {object_id} changed"
        );
    }
    let original_page = document.get_object(primary.page)?.as_dict()?;
    let output_page_dict = reloaded.get_object(output_page)?.as_dict()?;
    for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate"] {
        ensure!(
            original_page.get(key).ok() == output_page_dict.get(key).ok(),
            "page box key {:?} changed",
            String::from_utf8_lossy(key)
        );
    }

    let failure_report = verify_failure_atomicity(input, work)?;

    let companion_report = verify_companion_inputs(&root, work)?;

    Ok(format!(
        "input_sha256={}\noutput_sha256={output_sha}\ninput_len={} output_len={} appended_bytes={} max_input_object={} new_resources={} new_content={} new_font={} {failure_report}\n{companion_report}",
        sha256(before),
        before.len(),
        output_bytes.len(),
        output_bytes.len() - before.len(),
        max_id,
        primary.resources.0,
        primary.content.0,
        primary.font.0,
    ))
}

struct RewriteResult {
    before: Vec<u8>,
    output_bytes: Vec<u8>,
    document: Document,
    output_document: Document,
    page: ObjectId,
    original_resources: ObjectId,
    resources: ObjectId,
    font: ObjectId,
    content: ObjectId,
    form: Option<ObjectId>,
    max_id: u32,
}

#[derive(Clone, Copy)]
enum RewriteScenario {
    ReplaceText { x: i64, y: i64 },
    RewriteForm,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FailurePoint {
    ResourceCopy,
    FontAppend,
    Save,
}

fn incremental_rewrite(
    input: &Path,
    output: &Path,
    page_index: usize,
    scenario: RewriteScenario,
    failure: Option<FailurePoint>,
) -> Result<RewriteResult> {
    let before = fs::read(input)?;
    let document = Document::load(input).context("load input with lopdf")?;
    // The trailer /Size includes free/deleted xref slots that lopdf omits from
    // its live-object max_id. New objects must start after that ceiling.
    let max_id = document
        .trailer
        .get(b"Size")
        .ok()
        .and_then(|object| object.as_i64().ok())
        .and_then(|size| u32::try_from(size.saturating_sub(1)).ok())
        .unwrap_or(document.max_id);
    let page = document
        .get_pages()
        .into_iter()
        .nth(page_index)
        .map(|(_, id)| id)
        .context("input has no requested page")?;
    let original_resources = document
        .get_object(page)?
        .as_dict()?
        .get(b"Resources")?
        .as_reference()?;
    let mut incremental = IncrementalDocument::create_from(before.clone(), document.clone());
    incremental.new_document.max_id = max_id;
    incremental.opt_clone_object_to_new_document(page)?;

    let mut resources_object = document.get_object(original_resources)?.clone();
    let resources = copy_resources(
        &mut incremental,
        resources_object.clone(),
        failure == Some(FailurePoint::ResourceCopy),
    )?;
    let font = append_font_marker(&mut incremental, failure == Some(FailurePoint::FontAppend))?;
    let form = if matches!(scenario, RewriteScenario::RewriteForm) {
        let form_ref = resources_object
            .as_dict()?
            .get(b"XObject")?
            .as_dict()?
            .get(b"X1")?
            .as_reference()?;
        let mut form_object = document.get_object(form_ref)?.clone();
        let form_resources_ref = form_object
            .as_stream()?
            .dict
            .get(b"Resources")?
            .as_reference()?;
        let form_resources = incremental
            .new_document
            .add_object(document.get_object(form_resources_ref)?.clone());
        incremental
            .new_document
            .get_object_mut(form_resources)?
            .as_dict_mut()?
            .get_mut(b"Font")?
            .as_dict_mut()?
            .set("F2", font);
        let stream = form_object.as_stream_mut()?;
        stream.dict.set("Resources", form_resources);
        stream.dict.set(
            "BBox",
            vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(72),
                Object::Integer(16),
            ],
        );
        stream.set_plain_content(b"BT\n/F2 12 Tf\n1 0 0 1 0 3 Tm\n(FORM COW) Tj\nET\n".to_vec());
        let new_form = incremental.new_document.add_object(form_object);
        resources_object
            .as_dict_mut()?
            .get_mut(b"XObject")?
            .as_dict_mut()?
            .set("X1", new_form);
        Some(new_form)
    } else {
        None
    };
    resources_object
        .as_dict_mut()?
        .get_mut(b"Font")?
        .as_dict_mut()?
        .set("F2", font);
    *incremental.new_document.get_object_mut(resources)? = resources_object;
    incremental
        .new_document
        .get_object_mut(page)?
        .as_dict_mut()?
        .set("Resources", resources);
    let content_id = match scenario {
        RewriteScenario::ReplaceText { x, y } => {
            let content = format!("BT\n/F2 12 Tf\n1 0 0 1 {x} {y} Tm\n(POC) Tj\nET\n").into_bytes();
            let content_id = incremental.new_document.add_object(Stream::new(
                dictionary! { "Length" => Object::Integer(content.len() as i64) },
                content,
            ));
            incremental
                .new_document
                .get_object_mut(page)?
                .as_dict_mut()?
                .set("Contents", content_id);
            content_id
        }
        RewriteScenario::RewriteForm => document
            .get_object(page)?
            .as_dict()?
            .get(b"Contents")?
            .as_reference()?,
    };
    let output_bytes = save_incremental(
        &mut incremental,
        output,
        failure == Some(FailurePoint::Save),
    )?;
    let output_document = Document::load(output).context("reload incremental output")?;
    Ok(RewriteResult {
        before,
        output_bytes,
        document,
        output_document,
        page,
        original_resources,
        resources,
        font,
        content: content_id,
        form,
        max_id,
    })
}

fn verify_companion_inputs(root: &Path, work: &Path) -> Result<String> {
    let objstm_path =
        root.join("corpus/fixtures/unit-write-04-xobj-in-objstm/unit-write-04-xobj-in-objstm.pdf");
    let objstm_bytes = fs::read(&objstm_path)?;
    ensure!(objstm_bytes.windows(13).any(|w| w == b"/Type /ObjStm"));
    let objstm_output = work.join("companion-objstm-output.pdf");
    let objstm_result = incremental_rewrite(
        &objstm_path,
        &objstm_output,
        0,
        RewriteScenario::RewriteForm,
        None,
    )?;
    ensure!(
        objstm_result
            .output_bytes
            .starts_with(&objstm_result.before)
    );
    let objstm = &objstm_result.document;
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
    let new_form = objstm_result
        .form
        .context("ObjStm form was not rewritten")?;
    ensure!(new_form.0 > objstm_result.max_id);
    let output_form = objstm_result
        .output_document
        .get_object(new_form)?
        .as_stream()?;
    ensure!(output_form.content.windows(8).any(|w| w == b"FORM COW"));

    let geometry_path = root.join(
        "corpus/fixtures/unit-geom-05-nonzero-origin-boxes/unit-geom-05-nonzero-origin-boxes.pdf",
    );
    let geometry_output = work.join("companion-geometry-output.pdf");
    let geometry_result = incremental_rewrite(
        &geometry_path,
        &geometry_output,
        0,
        RewriteScenario::ReplaceText { x: 150, y: 220 },
        None,
    )?;
    ensure!(
        geometry_result
            .output_bytes
            .starts_with(&geometry_result.before)
    );
    let geometry = &geometry_result.document;
    let geometry_page = geometry
        .get_pages()
        .into_iter()
        .next()
        .context("geometry fixture has no page")?
        .1;
    let geometry_dict = geometry.get_object(geometry_page)?.as_dict()?;
    ensure!(geometry_dict.get(b"MediaBox")?.as_array()?.len() == 4);
    ensure!(geometry_dict.get(b"CropBox")?.as_array()?.len() == 4);
    let geometry_output_page = geometry_result
        .output_document
        .get_pages()
        .into_iter()
        .next()
        .context("geometry output has no page")?
        .1;
    let geometry_output_dict = geometry_result
        .output_document
        .get_object(geometry_output_page)?
        .as_dict()?;
    for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate"] {
        ensure!(geometry_dict.get(key).ok() == geometry_output_dict.get(key).ok());
    }

    let generation_path = root.join(
        "corpus/fixtures/unit-write-03-resources-gen-nonzero/unit-write-03-resources-gen-nonzero.pdf",
    );
    let generation_output = work.join("companion-generation-output.pdf");
    let generation_result = incremental_rewrite(
        &generation_path,
        &generation_output,
        0,
        RewriteScenario::ReplaceText { x: 72, y: 120 },
        None,
    )?;
    ensure!(
        generation_result
            .output_bytes
            .starts_with(&generation_result.before)
    );
    let generation = &generation_result.document;
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
    let generation_output_page = generation_result
        .output_document
        .get_pages()
        .into_iter()
        .next()
        .context("generation output has no page")?
        .1;
    ensure!(
        generation_result
            .output_document
            .get_object(generation_output_page)?
            .as_dict()?
            .get(b"Resources")?
            .as_reference()?
            != (4, 7)
    );
    ensure!(
        generation_result.output_document.get_object((4, 7))? == generation.get_object((4, 7))?
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
    let free_output = work.join("companion-free-output.pdf");
    let free_result = incremental_rewrite(
        &free_path,
        &free_output,
        0,
        RewriteScenario::ReplaceText { x: 72, y: 120 },
        None,
    )?;
    ensure!(free_result.output_bytes.starts_with(&free_result.before));
    let free = &free_result.document;
    ensure!(!object_numbers(free).contains(&10));
    ensure!(free_result.resources.0 > free_result.max_id);
    ensure!(
        free_result.resources.0 != 10 && free_result.font.0 != 10 && free_result.content.0 != 10,
        "free slot reused: max={} resources={} font={} content={}",
        free_result.max_id,
        free_result.resources.0,
        free_result.font.0,
        free_result.content.0
    );

    let shared_path = root
        .join("corpus/fixtures/unit-write-02-shared-resources/unit-write-02-shared-resources.pdf");
    let shared_output = work.join("companion-shared-output.pdf");
    let shared_result = incremental_rewrite(
        &shared_path,
        &shared_output,
        0,
        RewriteScenario::ReplaceText { x: 72, y: 120 },
        None,
    )?;
    let pages = shared_result.output_document.get_pages();
    let second_page = pages
        .get(&2)
        .copied()
        .context("shared output missing page 2")?;
    ensure!(
        shared_result
            .output_bytes
            .starts_with(&shared_result.before)
    );
    ensure!(
        shared_result
            .output_document
            .get_object(second_page)?
            .as_dict()?
            .get(b"Resources")?
            .as_reference()?
            == (5, 0)
    );
    ensure!(
        shared_result.output_document.get_object((5, 0))?
            == shared_result.document.get_object((5, 0))?
    );

    Ok(format!(
        "companions=objstm(form={}),geometry(boxes=preserved),generation(resources=4:7->{}:0),free(max={},new={}),shared(second_resources=5:0)",
        new_form.0, generation_result.resources.0, free_result.max_id, free_result.resources.0,
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

fn verify_failure_atomicity(input: &Path, work: &Path) -> Result<String> {
    let known_good = fs::read(input)?;
    let cases = [
        ("resource", FailurePoint::ResourceCopy),
        ("font", FailurePoint::FontAppend),
        ("save", FailurePoint::Save),
    ];
    let mut errors = Vec::new();

    for (name, failure) in cases {
        let target = work.join(format!("failure-{name}.pdf"));
        fs::write(&target, &known_good)?;
        let before = sha256(&known_good);
        let error = match incremental_rewrite(
            input,
            &target,
            0,
            RewriteScenario::ReplaceText { x: 72, y: 120 },
            Some(failure),
        ) {
            Ok(_) => return Err(anyhow!("injected {name} failure unexpectedly succeeded")),
            Err(error) => error,
        };
        ensure!(
            sha256(&fs::read(&target)?) == before,
            "{name} failure replaced its actual destination"
        );
        errors.push(format!("{name}_failure={error:#}"));
    }

    let temp = work.join(format!(".{}.tmp", std::process::id()));
    ensure!(!temp.exists(), "failed save left a temporary file");
    Ok(errors.join(" "))
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
