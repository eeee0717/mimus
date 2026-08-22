//! Deterministic raw-byte PDF fixtures for the M0 experiments.
//!
//! This module is corpus infrastructure, not a production PDF writer. It has
//! no PDF-library dependency and does not read expected manifest values.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail, ensure};
use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::hash;

pub const GENERATOR: &str = "corpus-exact-writer-v1";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Generate one exact fixture entirely in memory.
pub fn generate(fixture_id: &str, repo_root: &Path) -> Result<Vec<u8>> {
    match fixture_id {
        "unit-base-01-single-line" => single_line(repo_root),
        "unit-base-03-structured" => structured(repo_root),
        "unit-parse-01-ascii85" => filtered_text(fixture_id, repo_root, FilterRecipe::Ascii85),
        "unit-parse-02-cascade" => filtered_text(fixture_id, repo_root, FilterRecipe::Ascii85Flate),
        "unit-parse-03-lzw-earlychange" => {
            filtered_text(fixture_id, repo_root, FilterRecipe::LzwEarlyChange0)
        }
        "unit-parse-03-lzw-earlychange-1" => {
            filtered_text(fixture_id, repo_root, FilterRecipe::LzwEarlyChange1)
        }
        "unit-parse-04-contents-array-numeric-split" => contents_array_numeric_split(repo_root),
        "unit-parse-05-contents-array-string-parent" => contents_array_string_parent(repo_root),
        "unit-stream-00-malformed-parent" => malformed_stream_parent(repo_root),
        "unit-stream-01-bx-ex-unknown-op" => basic_text(
            fixture_id,
            repo_root,
            b"BX /Foo 1 2 3 SomeVendorOp EX\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n",
        ),
        "unit-stream-02-type3-d1" => type3_d1(repo_root),
        "unit-stream-04-type3-d0" => type3_d0(repo_root),
        "unit-stream-03-unknown-op-outside-bx" => basic_text(
            fixture_id,
            repo_root,
            b"BX\nSomeVendorOp\nEX\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n",
        ),
        "unit-stream-08-inline-image-EI-in-data" => inline_image(fixture_id, repo_root, false),
        "unit-stream-09-inline-image-no-L" => inline_image(fixture_id, repo_root, true),
        "unit-stream-10-inline-image-length" => basic_text(
            fixture_id,
            repo_root,
            b"q\nBI /W 8 /H 1 /BPC 8 /CS /G /L 8 ID\nABCDEFGH\nEI\nQ\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n",
        ),
        "unit-stream-11-inline-image-filtered-fallback" => basic_text(
            fixture_id,
            repo_root,
            b"q\nBI /W 8 /H 1 /BPC 8 /CS /G /F /AHx ID\n6060606060606060>\nEI\nQ\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n",
        ),
        "unit-font-01-std14-custom-widths" => std14_custom_widths(repo_root),
        "unit-cmap-01-identity-no-tounicode" => identity_cid_no_tounicode(repo_root),
        "unit-xobj-00-recursion-parent" => xobject_recursion_parent(repo_root),
        "unit-xobj-04-inherited-resources" => inherited_form_resources(repo_root),
        "unit-xobj-05-scope-parent" => xobject_scope_parent(repo_root),
        _ => bail!("exact fixture `{fixture_id}` is not implemented"),
    }
}

const STANDARD_CONTENT: &[u8] = b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n";

enum FilterRecipe {
    Ascii85,
    Ascii85Flate,
    LzwEarlyChange0,
    LzwEarlyChange1,
}

fn filtered_text(fixture_id: &str, repo_root: &Path, recipe: FilterRecipe) -> Result<Vec<u8>> {
    let (dictionary, encoded) = match recipe {
        FilterRecipe::Ascii85 => (
            "/Filter /ASCII85Decode".to_string(),
            ascii85(STANDARD_CONTENT),
        ),
        FilterRecipe::Ascii85Flate => {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
            encoder.write_all(STANDARD_CONTENT)?;
            let compressed = encoder.finish()?;
            (
                "/Filter [/ASCII85Decode /FlateDecode]".to_string(),
                ascii85(&compressed),
            )
        }
        FilterRecipe::LzwEarlyChange0 => (
            "/Filter /LZWDecode /DecodeParms << /EarlyChange 0 >>".to_string(),
            lzw_encode(&lzw_transition_content(), 0),
        ),
        FilterRecipe::LzwEarlyChange1 => (
            "/Filter /LZWDecode /DecodeParms << /EarlyChange 1 >>".to_string(),
            lzw_encode(&lzw_transition_content(), 1),
        ),
    };
    basic_pdf(fixture_id, repo_root, "9 0 R", &[(dictionary, encoded)])
}

fn basic_text(fixture_id: &str, repo_root: &Path, content: &[u8]) -> Result<Vec<u8>> {
    basic_pdf(
        fixture_id,
        repo_root,
        "9 0 R",
        &[(String::new(), content.to_vec())],
    )
}

fn basic_pdf(
    fixture_id: &str,
    repo_root: &Path,
    contents: &str,
    streams: &[(String, Vec<u8>)],
) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new(fixture_id);
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents {contents} >>"
        )
        .as_bytes(),
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    for (dictionary, data) in streams {
        pdf.stream(dictionary.as_bytes(), data)?;
    }
    pdf.finish(1)
}

fn contents_array_numeric_split(repo_root: &Path) -> Result<Vec<u8>> {
    basic_pdf(
        "unit-parse-04-contents-array-numeric-split",
        repo_root,
        "[9 0 R 10 0 R]",
        &[
            (String::new(), b"q 1 0 0 1 10".to_vec()),
            (
                String::new(),
                b" 20 cm\nBT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET\nQ\n".to_vec(),
            ),
        ],
    )
}

fn contents_array_string_parent(repo_root: &Path) -> Result<Vec<u8>> {
    basic_pdf(
        "unit-parse-05-contents-array-string-parent",
        repo_root,
        "[9 0 R 10 0 R]",
        &[
            (
                String::new(),
                b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) ".to_vec(),
            ),
            (String::new(), b"(MIMUS) Tj ET\n".to_vec()),
        ],
    )
}

fn malformed_stream_parent(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-stream-00-malformed-parent");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 10 0 R >>")?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.object(b"<< /MimusFixturePadding true >>")?;
    pdf.stream(b"", STANDARD_CONTENT)?;
    pdf.stream(
        b"",
        b"q 1 2 3 4 5 6 7 cm BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET Q\n",
    )?;
    pdf.stream(
        b"",
        b"q 1 0 0 1 100 cm BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET Q\n",
    )?;
    pdf.stream(b"", b"Q Q BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET\n")?;
    pdf.stream(b"", b"BT /F1 12Tf 100 120Td (MIMUS) Tj ET\n")?;
    pdf.stream(
        b"",
        b"BT /F1 12 Tf 1 0 0 1 72 120 Tm 10.5.3 Tc (MIMUS) Tj ET\n",
    )?;
    let mut nested = b"BT /F1 12 Tf 1 0 0 1 72 120 Tm ".to_vec();
    nested.extend(std::iter::repeat_n(b'[', 512));
    nested.extend_from_slice(b"(MIMUS)");
    nested.extend(std::iter::repeat_n(b']', 512));
    nested.extend_from_slice(b" TJ ET\n");
    pdf.stream(b"", &nested)?;
    pdf.finish(1)
}

fn type3_d1(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-stream-02-type3-d1");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 11 0 R >>")?;
    pdf.object(b"<< /Font << /F1 5 0 R /FT3 9 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.object(b"<< /Type /Font /Subtype /Type3 /Name /FT3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /M 10 0 R >> /Encoding << /Type /Encoding /Differences [77 /M] >> /FirstChar 77 /LastChar 77 /Widths [1000] /Resources << >> >>")?;
    pdf.stream(b"", b"1000 0 0 0 1000 1000 d1\n0 0 1000 1000 re f\n")?;
    pdf.stream(b"", b"BT /FT3 12 Tf 1 0 0 1 72 120 Tm (M) Tj ET\n")?;
    pdf.finish(1)
}

fn type3_d0(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-stream-04-type3-d0");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 11 0 R >>")?;
    pdf.object(b"<< /Font << /F1 5 0 R /FT3 9 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.object(b"<< /Type /Font /Subtype /Type3 /Name /FT3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /M 10 0 R >> /Encoding << /Type /Encoding /Differences [77 /M] >> /FirstChar 77 /LastChar 77 /Widths [1000] /Resources << >> >>")?;
    pdf.stream(b"", b"1000 0 d0\n0 0 1000 1000 re f\n")?;
    pdf.stream(b"", b"BT /FT3 12 Tf 1 0 0 1 72 120 Tm (M) Tj ET\n")?;
    pdf.finish(1)
}

fn inline_image(fixture_id: &str, repo_root: &Path, unknown_after: bool) -> Result<Vec<u8>> {
    let mut content = if unknown_after {
        b"q\nBX\nBI /W 8 /H 8 /BPC 8 /CS /G ID\n".to_vec()
    } else {
        b"q\nBI /W 9 /H 2 /BPC 1 /CS /G ID\n".to_vec()
    };
    if unknown_after {
        let mut pixels = [96u8; 64];
        pixels[20..24].copy_from_slice(b" EI ");
        content.extend_from_slice(&pixels);
        content.extend_from_slice(b"\nEI\n1 SomeVendorOp\nEX\nQ\n");
    } else {
        content.extend_from_slice(b" EI ");
        content.extend_from_slice(b"\nEI\nQ\n");
    }
    content.extend_from_slice(STANDARD_CONTENT);
    basic_text(fixture_id, repo_root, &content)
}

fn std14_custom_widths(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-font-01-std14-custom-widths");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 10 0 R >>")?;
    pdf.object(b"<< /Font << /F0 5 0 R /F1 9 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.object(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /FirstChar 65 /LastChar 90 /Widths [1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000] >>")?;
    pdf.stream(b"", b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (AAAA) Tj ET\n")?;
    pdf.finish(1)
}

fn identity_cid_no_tounicode(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-cmap-01-identity-no-tounicode");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 11 0 R >>")?;
    pdf.object(b"<< /Font << /F0 5 0 R /F1 9 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSI+DejaVuSans /Encoding /Identity-H /DescendantFonts [10 0 R] >>")?;
    pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSI+DejaVuSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 6 0 R /W [6 [295 863 0 635 0 732]] /CIDToGIDMap /Identity >>")?;
    pdf.stream(
        b"",
        b"BT /F1 12 Tf 1 0 0 1 72 120 Tm <000700060007000B0009> Tj ET\n",
    )?;
    pdf.finish(1)
}

fn xobject_recursion_parent(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-xobj-00-recursion-parent");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 15 0 R >>")?;
    pdf.object(
        b"<< /Font << /F1 5 0 R >> /XObject << /X0 10 0 R /X1 11 0 R /X2 12 0 R /X3 14 0 R >> >>",
    )?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.object(b"<< /MimusFixturePadding true >>")?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Resources << /Font << /F1 5 0 R >> >>",
        STANDARD_CONTENT,
    )?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /X1 11 0 R >> >>",
        b"/X1 Do\n",
    )?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /B 13 0 R >> >>",
        b"/B Do\n",
    )?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /A 12 0 R >> >>",
        b"/A Do\n",
    )?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /Matrix [2 0 0 2 10 20] /Resources << /Font << /F1 5 0 R >> >>",
        STANDARD_CONTENT,
    )?;
    pdf.stream(b"", b"q /X0 Do Q\n")?;
    pdf.finish(1)
}

fn inherited_form_resources(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-xobj-04-inherited-resources");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 12 0 R >>")?;
    pdf.object(b"<< /Font << /F1 9 0 R >> /XObject << /Outer 10 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.object(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Matrix [1 0 0 1 20 30] /Resources << /Font << /F1 13 0 R >> /XObject << /Inner 11 0 R >> >>",
        b"q 1 0 0 1 3 4 cm /Inner Do Q\n",
    )?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Matrix [1 0 0 1 5 7]",
        b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (III) Tj ET\n",
    )?;
    pdf.stream(
        b"",
        b"q 1 0 0 1 10 15 cm /Outer Do Q\nBT /F1 12 Tf 1 0 0 1 72 80 Tm (IIIH) Tj ET\n",
    )?;
    pdf.object(b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>")?;
    pdf.finish(1)
}

fn xobject_scope_parent(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-xobj-05-scope-parent");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 10 0 R >>")?;
    pdf.object(b"<< /Font << /F1 5 0 R >> /XObject << /X0 9 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(b"/Type /XObject /Subtype /Form /BBox [0 0 10 10]", b"q Q\n")?;
    pdf.stream(
        b"",
        b"q 1 0 0 1 50 0 cm /X0 Do Q\nBT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET\n",
    )?;
    pdf.finish(1)
}

fn ascii85(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut padded = [0u8; 4];
        padded[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from_be_bytes(padded);
        if chunk.len() == 4 && value == 0 {
            output.push(b'z');
            continue;
        }
        let mut digits = [0u8; 5];
        let mut value = value;
        for digit in digits.iter_mut().rev() {
            *digit = (value % 85) as u8 + b'!';
            value /= 85;
        }
        output.extend_from_slice(&digits[..chunk.len() + 1]);
    }
    output.extend_from_slice(b"~>");
    output
}

fn lzw_transition_content() -> Vec<u8> {
    let mut output = Vec::with_capacity(440);
    output.push(b'%');
    let mut state = 0x6d69_6d75u32;
    for _ in 0..384 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        output.push(33 + ((state >> 16) % 94) as u8);
    }
    output.push(b'\n');
    output.extend_from_slice(STANDARD_CONTENT);
    output
}

fn lzw_encode(bytes: &[u8], early_change: u16) -> Vec<u8> {
    use std::collections::BTreeMap;

    let mut dictionary: BTreeMap<Vec<u8>, u16> =
        (0u16..=255).map(|byte| (vec![byte as u8], byte)).collect();
    let mut next_code = 258u16;
    let mut writer = VariableWidthWriter::default();
    let mut width = 9u8;
    let mut pending_width_increase = false;
    writer.write(256, width);
    let Some((&first, rest)) = bytes.split_first() else {
        writer.write(257, width);
        return writer.finish();
    };
    let mut current = vec![first];
    for &byte in rest {
        let mut extended = current.clone();
        extended.push(byte);
        if dictionary.contains_key(&extended) {
            current = extended;
        } else {
            writer.write(dictionary[&current], width);
            if pending_width_increase {
                width += 1;
                pending_width_increase = false;
            }
            if next_code < 4096 {
                dictionary.insert(extended, next_code);
                if width < 12 && next_code + early_change == (1u16 << width) - 1 {
                    pending_width_increase = true;
                }
                next_code += 1;
            }
            current.clear();
            current.push(byte);
        }
    }
    writer.write(dictionary[&current], width);
    if pending_width_increase {
        width += 1;
    }
    writer.write(257, width);
    writer.finish()
}

#[derive(Default)]
struct VariableWidthWriter {
    output: Vec<u8>,
    buffer: u32,
    bits: u8,
}

impl VariableWidthWriter {
    fn write(&mut self, code: u16, width: u8) {
        self.buffer = (self.buffer << width) | u32::from(code);
        self.bits += width;
        while self.bits >= 8 {
            self.bits -= 8;
            self.output.push((self.buffer >> self.bits) as u8);
            self.buffer &= (1u32 << self.bits).wrapping_sub(1);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.output.push((self.buffer << (8 - self.bits)) as u8);
        }
        self.output
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
