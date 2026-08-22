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
         /FirstChar 32 /LastChar 85 /Widths [{widths}] /FontDescriptor 6 0 R \
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
9 beginbfchar\n\
<20> <0020>\n\
<43> <0043>\n\
<45> <0045>\n\
<49> <0049>\n\
<4D> <004D>\n\
<52> <0052>\n\
<53> <0053>\n\
<54> <0054>\n\
<55> <0055>\n\
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
}

impl RawPdf {
    fn new(fixture_id: &str) -> Self {
        Self {
            fixture_id: fixture_id.to_string(),
            bytes: b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec(),
            offsets: Vec::new(),
        }
    }

    fn object(&mut self, body: &[u8]) -> Result<u32> {
        ensure!(!body.is_empty(), "indirect object body must not be empty");
        let number = u32::try_from(self.offsets.len() + 1).context("too many PDF objects")?;
        self.offsets.push(self.bytes.len());
        self.bytes
            .extend_from_slice(format!("{number} 0 obj\n").as_bytes());
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
        for offset in &self.offsets {
            ensure!(
                *offset <= 9_999_999_999,
                "PDF offset exceeds classic xref width"
            );
            self.bytes
                .extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
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
