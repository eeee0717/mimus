//! Deterministic raw-byte PDF fixtures for the M0 experiments.
//!
//! This module is corpus infrastructure, not a production PDF writer. It has
//! no PDF-library dependency and does not read expected manifest values.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail, ensure};

use crate::hash;

pub const GENERATOR: &str = "corpus-exact-writer-v1";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Generate one exact fixture entirely in memory.
pub fn generate(fixture_id: &str, repo_root: &Path) -> Result<Vec<u8>> {
    match fixture_id {
        "unit-base-01-single-line" => single_line(repo_root),
        "unit-base-03-structured" => structured(repo_root),
        "unit-write-01-bookmarks-rich" => {
            structured_variant(repo_root, "unit-write-01-bookmarks-rich")
        }
        "unit-write-02-shared-resources" => shared_resources(repo_root),
        "unit-write-03-resources-gen-nonzero" => shared_resources_generation(repo_root),
        "unit-geom-05-nonzero-origin-boxes" => nonzero_origin_boxes(repo_root),
        "unit-cmap-02-mixed-codespace" => mixed_codespace(repo_root),
        "unit-xobj-05-singular-ctm" => singular_ctm(repo_root),
        "unit-write-04-xobj-in-objstm" => xobject_in_object_stream(repo_root),
        "unit-write-05-indirect-resources-objstm" => resources_in_object_stream(repo_root),
        "unit-parse-11-outline-siblings" => outline_siblings(repo_root),
        "unit-write-06-free-object-slot" => free_object_slot(repo_root),
        _ => bail!("exact fixture `{fixture_id}` is not implemented"),
    }
}

/// Generate completely in memory, then atomically replace `target`.
pub fn write_atomic(fixture_id: &str, repo_root: &Path, target: &Path) -> Result<String> {
    let bytes = generate(fixture_id, repo_root)?;
    write_bytes_atomic(&bytes, target)
}

pub(crate) fn write_bytes_atomic(bytes: &[u8], target: &Path) -> Result<String> {
    let parent = target
        .parent()
        .with_context(|| format!("output path has no parent: {}", target.display()))?;
    ensure!(
        parent.is_dir(),
        "output directory does not exist: {}",
        parent.display()
    );
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("output file name is not UTF-8: {}", target.display()))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));

    let write_result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("create temporary PDF {}", temp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write temporary PDF {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("flush temporary PDF {}", temp.display()))?;
        drop(file);
        std::fs::rename(&temp, target).with_context(|| {
            format!(
                "atomically replace {} with {}",
                target.display(),
                temp.display()
            )
        })?;
        Ok(())
    })();

    if let Err(error) = write_result {
        if temp.exists() {
            std::fs::remove_file(&temp)
                .with_context(|| format!("{error:#}; remove partial PDF {}", temp.display()))?;
        }
        return Err(error);
    }

    Ok(hash::of_bytes(bytes))
}

fn single_line(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-base-01-single-line");

    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 9 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(b"", b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n")?;

    pdf.finish(1)
}

fn structured(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-base-03-structured");

    pdf.object(
        b"<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R /PageMode /UseOutlines /Names << /Dests 16 0 R >> /AcroForm << /Fields [14 0 R] /DR 4 0 R /DA (/F1 10 Tf 0 g) /NeedAppearances true >> /OCProperties << /OCGs [15 0 R] /D << /Order [15 0 R] /ON [15 0 R] >> >> >>",
    )?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 18 0 R /Annots [12 0 R 13 0 R 14 0 R] >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> /Properties << /Layer 15 0 R >> >>")?;
    pdf.object(font_dictionary(17).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.object(b"<< /Type /Outlines /First 9 0 R /Last 9 0 R /Count 1 >>")?;
    pdf.object(
        b"<< /Title (Exact destination) /Parent 8 0 R /First 10 0 R /Last 10 0 R /Count -2 /Dest [3 0 R /XYZ 72 120 1] /C [0 0.4 0.8] /F 2 >>",
    )?;
    pdf.object(
        b"<< /Title (Named destination) /Parent 9 0 R /First 11 0 R /Last 11 0 R /Count 1 /Dest (body) >>",
    )?;
    pdf.object(
        b"<< /Title (URI action) /Parent 10 0 R /A << /S /URI /URI (https://example.com/mimus/bookmark) >> >>",
    )?;
    pdf.object(
        b"<< /Type /Annot /Subtype /Link /Rect [72 90 190 106] /Border [0 0 0] /A << /S /URI /URI (https://example.com/mimus) >> >>",
    )?;
    pdf.object(
        b"<< /Type /Annot /Subtype /Text /Rect [205 112 225 132] /Contents (MIMUS note) /Name /Comment >>",
    )?;
    pdf.object(
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (sample) /V () /Rect [72 60 190 78] /P 3 0 R /F 4 /DA (/F1 10 Tf 0 g) >>",
    )?;
    pdf.object(b"<< /Type /OCG /Name (MIMUS Layer) >>")?;
    pdf.object(b"<< /Names [(body) [3 0 R /XYZ 72 120 1]] >>")?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(
        b"",
        b"/OC /Layer BDC\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS STRUCTURE) Tj\nET\nEMC\n",
    )?;

    pdf.finish(1)
}

fn structured_variant(repo_root: &Path, fixture_id: &str) -> Result<Vec<u8>> {
    let mut bytes = structured(repo_root)?;
    let original = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n";
    ensure!(
        bytes.starts_with(original),
        "structured baseline header changed"
    );
    // The rich graph is deliberately byte-identical after the fixture-specific
    // trailer ID; this keeps the writer contract auditable while giving the
    // experiment its own named baseline.
    let old_id = hash::trailer_id_hex("unit-base-03-structured");
    let new_id = hash::trailer_id_hex(fixture_id);
    let old = format!("/ID [<{old_id}> <{old_id}>]");
    let new = format!("/ID [<{new_id}> <{new_id}>]");
    let old = old.as_bytes();
    let position = bytes
        .windows(old.len())
        .position(|window| window == old)
        .context("structured baseline trailer ID missing")?;
    bytes.splice(position..position + old.len(), new.into_bytes());
    Ok(bytes)
}

fn simple_font_page(
    repo_root: &Path,
    fixture_id: &str,
    media_box: [i32; 4],
    crop_box: Option<[i32; 4]>,
    resources_reference: &str,
    content: &[u8],
    generation: u16,
) -> Result<Vec<u8>> {
    simple_font_page_cmap(
        repo_root,
        fixture_id,
        media_box,
        crop_box,
        resources_reference,
        content,
        generation,
        to_unicode(),
    )
}

#[allow(clippy::too_many_arguments)]
fn simple_font_page_cmap(
    repo_root: &Path,
    fixture_id: &str,
    media_box: [i32; 4],
    crop_box: Option<[i32; 4]>,
    resources_reference: &str,
    content: &[u8],
    generation: u16,
    cmap: &[u8],
) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new(fixture_id);
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    let crop = crop_box.map_or(String::new(), |b| {
        format!(" /CropBox [{} {} {} {}]", b[0], b[1], b[2], b[3])
    });
    pdf.object(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [{} {} {} {}]{crop} /Resources {resources_reference} /Contents 9 0 R >>",
            media_box[0], media_box[1], media_box[2], media_box[3]
        )
        .as_bytes(),
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object_with_generation(font_dictionary(8).as_bytes(), generation)?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", cmap)?;
    pdf.stream(b"", content)?;
    pdf.finish(1)
}

fn shared_resources(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-write-02-shared-resources");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 5 0 R /Contents 10 0 R >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 5 0 R /Contents 11 0 R >>")?;
    pdf.object(b"<< /Font << /F1 6 0 R >> >>")?;
    pdf.object(font_dictionary_with_descriptor(9, 7).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 8 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(b"", b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n")?;
    pdf.stream(b"", b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUSC) Tj\nET\n")?;
    pdf.finish(1)
}

fn shared_resources_generation(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-write-03-resources-gen-nonzero");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 7 R /Contents 9 0 R >>",
    )?;
    pdf.object_with_generation(b"<< /Font << /F1 5 0 R >> >>", 7)?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(b"", b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n")?;
    pdf.finish(1)
}

fn nonzero_origin_boxes(repo_root: &Path) -> Result<Vec<u8>> {
    simple_font_page(
        repo_root,
        "unit-geom-05-nonzero-origin-boxes",
        [100, 100, 400, 300],
        Some([120, 120, 380, 280]),
        "4 0 R",
        b"BT\n/F1 12 Tf\n1 0 0 1 150 220 Tm\n(MIMUS) Tj\nET\n",
        0,
    )
}

fn mixed_codespace(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-cmap-02-mixed-codespace");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 11 0 R >>")?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSI+DejaVuSans /Encoding 9 0 R /DescendantFonts [6 0 R] /ToUnicode 10 0 R >>")?;
    pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSI+DejaVuSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /CIDToGIDMap /Identity /W [3 [600] 6 [600] 7 [600] 9 [600] 11 [600]] >>")?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 8 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", mixed_encoding())?;
    pdf.stream(b"/Type /CMap", mixed_to_unicode())?;
    pdf.stream(
        b"",
        b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n<070681400B09> Tj\nET\n",
    )?;
    pdf.finish(1)
}

fn singular_ctm(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-xobj-05-singular-ctm");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 9 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> /XObject << /X1 10 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(
        b"",
        b"q\n0 0 0 0 0 0 cm\n/X1 Do\nQ\nBT\n/F1 12 Tf\n1 0 0 1 100 100 Tm\n(MIMUS) Tj\nET\n",
    )?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 4 0 R",
        b"q\n0 0 0 0 0 0 cm\nBT\n/F1 12 Tf\n1 0 0 1 0 0 Tm\n(FORM) Tj\nET\nQ\n",
    )?;
    pdf.finish(1)
}

fn xobject_in_object_stream(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-write-04-xobj-in-objstm");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 9 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> /XObject << /X1 12 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(
        b"",
        b"q\n/X1 Do\nQ\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n",
    )?;
    let form_content = b"BT\n/F1 12 Tf\n1 0 0 1 0 0 Tm\n(FORM) Tj\nET\n";
    let form_body = format!(
        "<< /Length {} /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 11 0 R >>\nstream\n{}endstream",
        form_content.len(),
        String::from_utf8_lossy(form_content)
    );
    pdf.finish_with_object_stream_with_tail(
        1,
        11,
        b"<< /Font << /F1 5 0 R >> >>",
        form_body.as_bytes(),
    )
}

fn resources_in_object_stream(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-write-05-indirect-resources-objstm");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 11 0 R /Contents 9 0 R >>")?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary_without_widths(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(b"", b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n")?;
    pdf.finish_with_object_stream(1, 11, b"<< /Font << /F1 5 0 R >> >>")
}

fn outline_siblings(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-parse-11-outline-siblings");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 12 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.object(b"<< /Type /Outlines /First 10 0 R /Last 11 0 R /Count 2 >>")?;
    pdf.object(
        b"<< /Title (First sibling) /Parent 9 0 R /Next 11 0 R /Dest [3 0 R /XYZ 72 120 1] >>",
    )?;
    pdf.object(
        b"<< /Title (Second sibling) /Parent 9 0 R /Prev 10 0 R /Dest [3 0 R /XYZ 72 100 1] >>",
    )?;
    pdf.stream(b"", b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n")?;
    pdf.finish(1)
}

fn free_object_slot(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-write-06-free-object-slot");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 9 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(b"", b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n")?;
    pdf.finish_with_free_object(1, 10)
}

const FONT_SHA256: &str = "6e1e40974dce5dca579f3f191dd7dcc9953e6e04165d69f36d01aa8242a24735";

fn pinned_font(repo_root: &Path) -> Result<Vec<u8>> {
    let path = repo_root.join("corpus/fonts/MimusExact.ttf");
    let bytes = std::fs::read(&path)
        .with_context(|| format!("read pinned exact-fixture font {}", path.display()))?;
    ensure!(
        hash::of_bytes(&bytes) == FONT_SHA256,
        "pinned exact-fixture font SHA-256 changed: {}",
        path.display()
    );
    Ok(bytes)
}

fn font_dictionary(to_unicode_object: u32) -> String {
    font_dictionary_with_descriptor(to_unicode_object, 6)
}

fn font_dictionary_with_descriptor(to_unicode_object: u32, descriptor_object: u32) -> String {
    // /Widths covers character codes 32 through 85. Only the nine glyphs in
    // the pinned subset have non-zero widths; values are hmtx * 1000 / 2048,
    // rounded to the nearest integer as required by this fixture contract.
    let widths = [
        318, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 698, 0, 632, 0, 0, 0, 295, 0, 0, 0, 863, 0, 0, 0, 0, 695, 635, 611, 732,
    ];
    let widths = widths
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<< /Type /Font /Subtype /TrueType /BaseFont /MIMUSI+DejaVuSans \
         /FirstChar 32 /LastChar 85 /Widths [{widths}] /FontDescriptor {descriptor_object} 0 R \
         /Encoding /WinAnsiEncoding /ToUnicode {to_unicode_object} 0 R >>"
    )
}

fn font_dictionary_without_widths(to_unicode_object: u32) -> String {
    format!(
        "<< /Type /Font /Subtype /TrueType /BaseFont /MIMUSI+DejaVuSans \
         /FirstChar 32 /LastChar 85 /FontDescriptor 6 0 R \
         /Encoding /WinAnsiEncoding /ToUnicode {to_unicode_object} 0 R >>"
    )
}

fn to_unicode() -> &'static [u8] {
    b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /MimusExact-UCS def\n\
/CMapType 2 def\n\
1 begincodespacerange\n\
<00> <FF>\n\
endcodespacerange\n\
4 beginbfchar\n\
<49> <0049>\n\
<4D> <004D>\n\
<53> <0053>\n\
<55> <0055>\n\
endbfchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end\n"
}

fn mixed_encoding() -> &'static [u8] {
    b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def\n\
/CMapName /MimusMixed-Encoding def\n\
/CMapType 1 def\n\
2 begincodespacerange\n\
<00> <80>\n\
<8140> <FEFE>\n\
endcodespacerange\n\
5 begincidchar\n\
<06> 6\n\
<07> 7\n\
<09> 9\n\
<0B> 11\n\
<8140> 7\n\
endcidchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end\n"
}

fn mixed_to_unicode() -> &'static [u8] {
    b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /MimusMixed-UCS def\n\
/CMapType 2 def\n\
2 begincodespacerange\n\
<00> <80>\n\
<8140> <FEFE>\n\
endcodespacerange\n\
5 beginbfchar\n\
<06> <0049>\n\
<07> <004D>\n\
<09> <0053>\n\
<0B> <0055>\n\
<8140> <004D>\n\
endbfchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end\n"
}

/// Minimal sequential indirect-object writer. Object numbers are assigned in
/// call order; callers cannot insert, reorder, or reuse a number accidentally.
struct RawPdf {
    fixture_id: String,
    bytes: Vec<u8>,
    offsets: Vec<usize>,
    generations: Vec<u16>,
}

impl RawPdf {
    fn new(fixture_id: &str) -> Self {
        Self {
            fixture_id: fixture_id.to_string(),
            bytes: b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec(),
            offsets: Vec::new(),
            generations: Vec::new(),
        }
    }

    fn object(&mut self, body: &[u8]) -> Result<u32> {
        self.object_with_generation(body, 0)
    }

    fn object_with_generation(&mut self, body: &[u8], generation: u16) -> Result<u32> {
        ensure!(!body.is_empty(), "indirect object body must not be empty");
        let number = u32::try_from(self.offsets.len() + 1).context("too many PDF objects")?;
        self.offsets.push(self.bytes.len());
        self.generations.push(generation);
        self.bytes
            .extend_from_slice(format!("{number} {generation} obj\n").as_bytes());
        self.bytes.extend_from_slice(body);
        if !body.ends_with(b"\n") {
            self.bytes.push(b'\n');
        }
        self.bytes.extend_from_slice(b"endobj\n");
        Ok(number)
    }

    fn stream(&mut self, dictionary_entries: &[u8], data: &[u8]) -> Result<u32> {
        let mut body = format!("<< /Length {}", data.len()).into_bytes();
        if !dictionary_entries.is_empty() {
            body.push(b' ');
            body.extend_from_slice(dictionary_entries);
        }
        body.extend_from_slice(b" >>\nstream\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"endstream");
        self.object(&body)
    }

    fn finish(mut self, root_object: u32) -> Result<Vec<u8>> {
        ensure!(root_object > 0, "root object must be indirect");
        ensure!(
            usize::try_from(root_object)
                .ok()
                .is_some_and(|n| n <= self.offsets.len()),
            "root object {root_object} was not written"
        );

        let xref_offset = self.bytes.len();
        let size = self.offsets.len() + 1;
        self.bytes
            .extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for (offset, generation) in self.offsets.iter().zip(&self.generations) {
            ensure!(
                *offset <= 9_999_999_999,
                "PDF offset exceeds classic xref width"
            );
            self.bytes
                .extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }

        let id = hash::trailer_id_hex(&self.fixture_id);
        self.bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {root_object} 0 R /ID [<{id}> <{id}>] >>\n\
                 startxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Ok(self.bytes)
    }

    fn finish_with_free_object(mut self, root_object: u32, free_object: u32) -> Result<Vec<u8>> {
        ensure!(free_object == self.offsets.len() as u32 + 1);
        let xref_offset = self.bytes.len();
        let size = self.offsets.len() + 2;
        self.bytes
            .extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for (offset, generation) in self.offsets.iter().zip(&self.generations) {
            ensure!(
                *offset <= 9_999_999_999,
                "PDF offset exceeds classic xref width"
            );
            self.bytes
                .extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
        self.bytes.extend_from_slice(b"0000000000 00000 f \n");
        let id = hash::trailer_id_hex(&self.fixture_id);
        self.bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {root_object} 0 R /ID [<{id}> <{id}>] >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Ok(self.bytes)
    }

    /// Finish with one compressed object and an xref stream. The object stream
    /// is intentionally tiny so independent tools can inspect it without any
    /// producer-specific assumptions.
    fn finish_with_object_stream(
        self,
        root_object: u32,
        compressed_object: u32,
        compressed_body: &[u8],
    ) -> Result<Vec<u8>> {
        self.finish_with_object_stream_and_tail(
            root_object,
            compressed_object,
            compressed_body,
            None,
        )
    }

    fn finish_with_object_stream_with_tail(
        self,
        root_object: u32,
        compressed_object: u32,
        compressed_body: &[u8],
        tail_body: &[u8],
    ) -> Result<Vec<u8>> {
        self.finish_with_object_stream_and_tail(
            root_object,
            compressed_object,
            compressed_body,
            Some(tail_body),
        )
    }

    fn finish_with_object_stream_and_tail(
        mut self,
        root_object: u32,
        compressed_object: u32,
        compressed_body: &[u8],
        tail_body: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        ensure!(compressed_object == self.offsets.len() as u32 + 2);
        let object_stream = self.offsets.len() as u32 + 1;
        let header = format!("{compressed_object} 0 ");
        let first = header.len();
        let mut stream = header.into_bytes();
        stream.extend_from_slice(compressed_body);
        let object_stream_number = u32::try_from(self.offsets.len() + 1)?;
        ensure!(object_stream_number == object_stream);
        self.offsets.push(self.bytes.len());
        self.generations.push(0);
        self.bytes
            .extend_from_slice(format!("{object_stream} 0 obj\n").as_bytes());
        self.bytes.extend_from_slice(
            format!(
                "<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                stream.len()
            )
            .as_bytes(),
        );
        self.bytes.extend_from_slice(&stream);
        self.bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let tail_object = if let Some(body) = tail_body {
            let tail_object = object_stream + 2;
            self.offsets.push(self.bytes.len());
            self.generations.push(0);
            self.bytes
                .extend_from_slice(format!("{tail_object} 0 obj\n").as_bytes());
            self.bytes.extend_from_slice(body);
            if !body.ends_with(b"\n") {
                self.bytes.push(b'\n');
            }
            self.bytes.extend_from_slice(b"endobj\n");
            tail_object
        } else {
            object_stream + 1
        };
        let xref_object = tail_object + 1;
        let size = xref_object + 1;
        let xref_offset = self.bytes.len();
        let mut entries = Vec::with_capacity(size as usize * 7);
        entries.extend_from_slice(&[0, 0, 0, 0, 0, 0xff, 0xff]);
        for object in 1..=object_stream {
            let offset = self.offsets[(object - 1) as usize] as u32;
            entries.push(1);
            entries.extend_from_slice(&offset.to_be_bytes());
            let generation = self.generations[(object - 1) as usize];
            entries.extend_from_slice(&generation.to_be_bytes());
        }
        entries.push(2);
        entries.extend_from_slice(&object_stream.to_be_bytes());
        entries.extend_from_slice(&[0, 0]);
        if tail_body.is_some() {
            // The compressed member has no physical offset in this vector,
            // so the regular tail object is one slot earlier than its number.
            let offset = self.offsets[(tail_object - 2) as usize] as u32;
            entries.push(1);
            entries.extend_from_slice(&offset.to_be_bytes());
            entries.extend_from_slice(&[0, 0]);
        }
        entries.push(1);
        entries.extend_from_slice(&(xref_offset as u32).to_be_bytes());
        entries.extend_from_slice(&[0, 0]);
        let id = hash::trailer_id_hex(&self.fixture_id);
        self.bytes.extend_from_slice(
            format!(
                "{xref_object} 0 obj\n<< /Type /XRef /Size {size} /W [1 4 2] /Root {root_object} 0 R /ID [<{id}> <{id}>] /Length {} >>\nstream\n",
                entries.len()
            )
            .as_bytes(),
        );
        self.bytes.extend_from_slice(&entries);
        self.bytes.extend_from_slice(b"\nendstream\nendobj\n");
        self.bytes
            .extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        Ok(self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn test_dir(label: &str) -> std::path::PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mimus-corpus-exact-{label}-{}-{sequence}",
            std::process::id()
        ))
    }

    #[test]
    fn single_line_recipe_has_a_fixed_auditable_byte_contract() {
        let first = generate("unit-base-01-single-line", &repo_root()).unwrap();
        let second = generate("unit-base-01-single-line", &repo_root()).unwrap();

        assert_eq!(first, second);
        assert!(first.starts_with(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj\n"));
        assert!(contains(&first, b"9 0 obj\n<< /Length "));
        assert!(contains(
            &first,
            b"stream\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj"
        ));
        assert!(contains(
            &first,
            b"/ID [<756e69742d626173652d30312d73696e> <756e69742d626173652d30312d73696e>]"
        ));

        let objects: Vec<usize> = (1..=9)
            .map(|number| {
                let marker = format!("\n{number} 0 obj\n");
                first
                    .windows(marker.len())
                    .position(|w| w == marker.as_bytes())
                    .unwrap_or_else(|| panic!("missing object {number}"))
            })
            .collect();
        assert!(objects.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn structured_recipe_has_the_fixed_rich_object_graph() {
        let first = generate("unit-base-03-structured", &repo_root()).unwrap();
        let second = generate("unit-base-03-structured", &repo_root()).unwrap();

        assert_eq!(first, second);
        for number in 1..=18 {
            assert!(
                contains(&first, format!("\n{number} 0 obj\n").as_bytes()),
                "missing object {number}"
            );
        }
        for expected in [
            b"/Outlines 8 0 R".as_slice(),
            b"/First 9 0 R /Last 9 0 R /Count 1".as_slice(),
            b"/First 10 0 R /Last 10 0 R /Count -2".as_slice(),
            b"/Dest [3 0 R /XYZ 72 120 1]".as_slice(),
            b"/Dest (body)".as_slice(),
            b"/URI (https://example.com/mimus/bookmark)".as_slice(),
            b"/Annots [12 0 R 13 0 R 14 0 R]".as_slice(),
            b"/Fields [14 0 R] /DR 4 0 R".as_slice(),
            b"/OCGs [15 0 R]".as_slice(),
            b"/Properties << /Layer 15 0 R >>".as_slice(),
            b"/Names [(body) [3 0 R /XYZ 72 120 1]]".as_slice(),
            b"stream\n/OC /Layer BDC\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS STRUCTURE) Tj\nET\nEMC\nendstream"
                .as_slice(),
        ] {
            assert!(
                contains(&first, expected),
                "missing byte contract fragment: {}",
                String::from_utf8_lossy(expected)
            );
        }
    }

    #[test]
    fn cmap_contracts_keep_simple_codes_single_byte_and_mixed_codes_explicit() {
        let simple = to_unicode();
        assert!(contains(simple, b"1 begincodespacerange\n<00> <FF>"));
        assert!(contains(simple, b"4 beginbfchar"));
        assert!(!contains(simple, b"5 beginbfchar"));
        assert!(contains(simple, b"<4D> <004D>"));

        let encoding = mixed_encoding();
        assert!(contains(encoding, b"<00> <80>"));
        assert!(contains(encoding, b"<8140> <FEFE>"));
        assert!(contains(encoding, b"5 begincidchar"));

        let mixed = mixed_to_unicode();
        assert!(contains(mixed, b"<8140> <004D>"));
        assert!(contains(mixed, b"5 beginbfchar"));
    }

    #[test]
    fn shared_resources_font_descriptor_references_the_embedded_font_stream() {
        let bytes = generate("unit-write-02-shared-resources", &repo_root()).unwrap();

        assert!(contains(&bytes, b"/FontFile2 8 0 R"));
        assert!(contains(&bytes, b"8 0 obj\n<< /Length "));
    }

    #[test]
    fn a_failed_generation_preserves_the_existing_output_without_a_partial_file() {
        let dir = test_dir("generation-failure");
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("failed.pdf");
        std::fs::write(&output, b"known-good-existing-output").unwrap();

        let error = write_atomic("unit-unknown-00", &repo_root(), &output).unwrap_err();

        assert!(error.to_string().contains("not implemented"), "{error:#}");
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"known-good-existing-output"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_atomic_replace_removes_its_temporary_file() {
        let dir = test_dir("replace-failure");
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("target.pdf");
        std::fs::create_dir(&output).unwrap();

        let error = write_bytes_atomic(b"complete-in-memory-pdf", &output).unwrap_err();

        assert!(
            error.to_string().contains("atomically replace"),
            "{error:#}"
        );
        assert!(output.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
