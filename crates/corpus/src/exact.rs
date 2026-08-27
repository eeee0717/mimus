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
        "unit-type-01-single-line-tight" => basic_text(
            fixture_id,
            repo_root,
            b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS MIMUS) Tj\nET\n",
        ),
        "unit-type-02-mixed-formula-slots" => basic_text(
            fixture_id,
            repo_root,
            b"BT\n/F1 12 Tf\n1 0 0 1 72 140 Tm\n(MIMUS) Tj\n1 0 0 1 72 100 Tm\n(MIMUS) Tj\nET\n",
        ),
        "unit-type-03-multiline-expansion" => basic_text(
            fixture_id,
            repo_root,
            b"BT\n/F1 12 Tf\n1 0 0 1 72 140 Tm\n(MIMUS) Tj\n1 0 0 1 72 126 Tm\n(MIMUS) Tj\n1 0 0 1 220 140 Tm\n(MIMUS) Tj\n1 0 0 1 220 126 Tm\n(MIMUS) Tj\n1 0 0 1 72 92 Tm\n(MIMUS) Tj\nET\n",
        ),
        "unit-type-04-inline-formula-flow" => basic_text(
            fixture_id,
            repo_root,
            b"BT\n/F1 12 Tf\n1 0 0 1 72 140 Tm\n(M) Tj\n(I) Tj\n(M) Tj\n(U) Tj\n(S) Tj\n1 0 0 1 72 126 Tm\n(M) Tj\n(I) Tj\n(M) Tj\n(U) Tj\n(S) Tj\nET\n",
        ),
        "unit-translation-01-section-title-number" => basic_text_with_page_size(
            fixture_id,
            repo_root,
            300,
            220,
            b"BT\n/F1 12 Tf\n1 0 0 1 125 195 Tm\n(MIMUS) Tj\n1 0 0 1 108 195 Tm\n(I) Tj\nET\n",
        ),
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
        "unit-parse-07-inherited-page-resources" => inherited_page_resources(repo_root),
        "unit-parse-indirect-filter" => indirect_filter(repo_root),
        "unit-parse-midtree-resources" => midtree_resources(repo_root),
        "unit-parse-m1-switchboard" => parse_m1_switchboard(repo_root),
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
        "unit-stream-11-inline-image-filtered-fallback" => {
            inline_image_filtered_fallback(repo_root)
        }
        "unit-stream-odd-hex" => odd_hex_identity(repo_root),
        "unit-stream-tr7-clip" => tr7_clip(repo_root),
        "unit-font-01-std14-custom-widths" => std14_custom_widths(repo_root),
        "unit-font-escaped-name" => escaped_font_name(repo_root),
        "unit-cmap-01-identity-no-tounicode" => identity_cid_no_tounicode(repo_root),
        "unit-cmap-embedded-ok" => embedded_cmap_ok(repo_root),
        "unit-cmap-identity-alias" => identity_cmap_alias(repo_root),
        "unit-cmap-predefined-gb" => predefined_gb_cmap(repo_root),
        "intg-cmap-mixed-degrade" => mixed_cmap_degradation(repo_root),
        "unit-align-01-independent-space-show" => {
            alignment_fixture(fixture_id, repo_root, AlignmentRecipe::IndependentSpace)
        }
        "unit-align-02-hyphen-marker" => {
            alignment_fixture(fixture_id, repo_root, AlignmentRecipe::HyphenMarker)
        }
        "unit-align-03-high-surrogate" => {
            alignment_composite_fixture(fixture_id, repo_root, "D835DC00")
        }
        "unit-align-04-ligature-expansion" => {
            alignment_fixture(fixture_id, repo_root, AlignmentRecipe::Ligature)
        }
        "unit-align-05-tounicode-noncharacter" => {
            alignment_fixture(fixture_id, repo_root, AlignmentRecipe::Noncharacter)
        }
        "unit-align-06-double-draw" => {
            alignment_fixture(fixture_id, repo_root, AlignmentRecipe::DoubleDraw)
        }
        "unit-align-07-weak-unicode-conflict" => {
            alignment_fixture(fixture_id, repo_root, AlignmentRecipe::WeakConflict)
        }
        "unit-align-08-actualtext-overlap" => {
            alignment_fixture(fixture_id, repo_root, AlignmentRecipe::ActualTextOverlap)
        }
        "unit-align-09-actualtext-disjoint" => {
            alignment_fixture(fixture_id, repo_root, AlignmentRecipe::ActualTextDisjoint)
        }
        "unit-xobj-00-recursion-parent" => xobject_recursion_parent(repo_root),
        "unit-xobj-04-inherited-resources" => inherited_form_resources(repo_root),
        "unit-xobj-05-scope-parent" => xobject_scope_parent(repo_root),
        "unit-xobj-depth-overflow" => xobject_depth_overflow(repo_root),
        "unit-xobj-m1-switchboard" => xobject_m1_switchboard(repo_root),
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
        "unit-doc-04-rotated-90" => geometry_text_page(
            repo_root,
            fixture_id,
            "",
            "/MediaBox [0 0 612 792]",
            b"BT\n/F1 12 Tf\n0 1 -1 0 100 700 Tm\n(M) Tj\nET\n",
            &[],
        ),
        "unit-doc-04-rotated-45" => geometry_text_page(
            repo_root,
            fixture_id,
            "",
            "/MediaBox [0 0 612 792]",
            b"BT\n/F1 12 Tf\n0.707107 0.707107 -0.707107 0.707107 100 700 Tm\n(M) Tj\nET\n",
            &[],
        ),
        "unit-doc-04-mirrored" => geometry_text_page(
            repo_root,
            fixture_id,
            "",
            "/MediaBox [0 0 612 792]",
            b"BT\n/F1 12 Tf\n-1 0 0 1 100 700 Tm\n(M) Tj\nET\n",
            &[],
        ),
        "unit-doc-04-skew-15" => geometry_text_page(
            repo_root,
            fixture_id,
            "",
            "/MediaBox [0 0 612 792]",
            b"BT\n/F1 12 Tf\n1 0 0.267949 1 100 700 Tm\n(M) Tj\nET\n",
            &[],
        ),
        "unit-doc-04-rotate90-compensated" => geometry_text_page(
            repo_root,
            fixture_id,
            "",
            "/MediaBox [0 0 612 792] /Rotate 90",
            b"BT\n/F1 12 Tf\n0 -1 1 0 100 700 Tm\n(M) Tj\nET\n",
            &[],
        ),
        "unit-doc-04-mixed-char" => geometry_text_page(
            repo_root,
            fixture_id,
            "",
            "/MediaBox [0 0 612 792]",
            b"BT\n/F1 12 Tf\n1 0 0 1 100 700 Tm\n(I) Tj\n0.707107 0.707107 -0.707107 0.707107 200 700 Tm\n(M) Tj\n1 0 0 1 300 700 Tm\n(S) Tj\nET\n",
            &[],
        ),
        "unit-geom-06-mediabox-double-space" => geometry_text_page(
            repo_root,
            fixture_id,
            "",
            "/MediaBox [0  000 612 792]",
            b"BT\n/F1 12 Tf\n1 0 0 1 100 700 Tm\n(M) Tj\nET\n",
            &[],
        ),
        "unit-geom-06-mediabox-indirect" => geometry_text_page(
            repo_root,
            fixture_id,
            "",
            "/MediaBox 10 0 R",
            b"BT\n/F1 12 Tf\n1 0 0 1 100 700 Tm\n(M) Tj\nET\n",
            &[b"[0 0 612 792]"],
        ),
        "unit-geom-08-cropbox-inherited" => geometry_text_page(
            repo_root,
            fixture_id,
            "/CropBox [50 50 562 742]",
            "/MediaBox [0 0 612 792]",
            b"BT\n/F1 12 Tf\n1 0 0 1 100 700 Tm\n(M) Tj\nET\n",
            &[],
        ),
        "unit-scan-01-image-only" => scan_document(fixture_id, repo_root, &[ScanPage::Image]),
        "unit-scan-02-invisible-ocr" => {
            scan_document(fixture_id, repo_root, &[ScanPage::ImageInvisibleText])
        }
        "unit-scan-03-visible-image-text" => {
            scan_document(fixture_id, repo_root, &[ScanPage::ImageVisibleText])
        }
        "unit-scan-04-title-page" => {
            scan_document(fixture_id, repo_root, &[ScanPage::Title])
        }
        "unit-scan-05-hidden-watermark" => {
            scan_document(fixture_id, repo_root, &[ScanPage::TextWatermark])
        }
        "intg-scan-06-blank-middle" => scan_document(
            fixture_id,
            repo_root,
            &[ScanPage::Text, ScanPage::Blank, ScanPage::Text],
        ),
        "intg-scan-07-image-middle" => scan_document(
            fixture_id,
            repo_root,
            &[ScanPage::Text, ScanPage::Image, ScanPage::Text],
        ),
        "intg-scan-08-text-first" => scan_document(
            fixture_id,
            repo_root,
            &[
                ScanPage::Text,
                ScanPage::Image,
                ScanPage::Image,
                ScanPage::Image,
            ],
        ),
        "intg-scan-09-text-last" => scan_document(
            fixture_id,
            repo_root,
            &[
                ScanPage::Image,
                ScanPage::Image,
                ScanPage::Image,
                ScanPage::Text,
            ],
        ),
        "intg-scan-10-nine-of-ten" => {
            let mut pages = vec![ScanPage::Image; 9];
            pages.push(ScanPage::Text);
            scan_document(fixture_id, repo_root, &pages)
        }
        "intg-scan-11-four-of-five" => {
            let mut pages = vec![ScanPage::Image; 4];
            pages.push(ScanPage::Text);
            scan_document(fixture_id, repo_root, &pages)
        }
        "intg-scan-12-image-with-blank-backs" => {
            let mut pages = vec![ScanPage::Image];
            pages.extend(std::iter::repeat_n(ScanPage::Blank, 9));
            scan_document(fixture_id, repo_root, &pages)
        }
        _ => bail!("exact fixture `{fixture_id}` is not implemented"),
    }
}

#[derive(Clone, Copy)]
enum ScanPage {
    Blank,
    Image,
    Text,
    Title,
    ImageInvisibleText,
    ImageVisibleText,
    TextWatermark,
}

impl ScanPage {
    fn uses_font(self) -> bool {
        matches!(
            self,
            Self::Text
                | Self::Title
                | Self::ImageInvisibleText
                | Self::ImageVisibleText
                | Self::TextWatermark
        )
    }

    fn uses_image(self) -> bool {
        matches!(
            self,
            Self::Image | Self::ImageInvisibleText | Self::ImageVisibleText
        )
    }

    fn content(self) -> Option<&'static [u8]> {
        match self {
            Self::Blank => None,
            Self::Image => Some(b"q\n300 0 0 200 0 0 cm\n/Im1 Do\nQ\n"),
            Self::Text => Some(STANDARD_CONTENT),
            Self::Title => {
                Some(b"BT\n/F1 24 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n")
            }
            Self::ImageInvisibleText => Some(
                b"q\n300 0 0 200 0 0 cm\n/Im1 Do\nQ\nBT\n/F1 12 Tf\n3 Tr\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n",
            ),
            Self::ImageVisibleText => Some(
                b"q\n300 0 0 200 0 0 cm\n/Im1 Do\nQ\nBT\n/F1 12 Tf\n0 Tr\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n",
            ),
            Self::TextWatermark => Some(
                b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\nBT\n/F1 6 Tf\n3 Tr\n1 0 0 1 72 60 Tm\n(MIMUS) Tj\nET\n",
            ),
        }
    }
}

fn scan_document(fixture_id: &str, repo_root: &Path, pages: &[ScanPage]) -> Result<Vec<u8>> {
    ensure!(
        !pages.is_empty(),
        "scan fixture must have at least one page"
    );
    let uses_font = pages.iter().copied().any(ScanPage::uses_font);
    let uses_image = pages.iter().copied().any(ScanPage::uses_image);
    let page_count = u32::try_from(pages.len())?;
    let resources_object = page_count + 3;
    let mut next_object = resources_object + 1;
    let font_objects = uses_font.then(|| {
        let objects = [
            next_object,
            next_object + 1,
            next_object + 2,
            next_object + 3,
        ];
        next_object += 4;
        objects
    });
    let image_object = uses_image.then(|| {
        let object = next_object;
        next_object += 1;
        object
    });
    let content_objects: Vec<Option<u32>> = pages
        .iter()
        .map(|page| {
            page.content().map(|_| {
                let object = next_object;
                next_object += 1;
                object
            })
        })
        .collect();

    let mut pdf = RawPdf::new(fixture_id);
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", index + 3))
        .collect::<Vec<_>>()
        .join(" ");
    pdf.object(format!("<< /Type /Pages /Kids [{kids}] /Count {page_count} >>").as_bytes())?;
    for (index, content_object) in content_objects.iter().enumerate() {
        let contents =
            content_object.map_or(String::new(), |object| format!(" /Contents {object} 0 R"));
        let object = pdf.object(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources {resources_object} 0 R{contents} >>"
            )
            .as_bytes(),
        )?;
        ensure!(object == u32::try_from(index)? + 3);
    }

    let mut resources = String::from("<<");
    if let Some([font, ..]) = font_objects {
        resources.push_str(&format!(" /Font << /F1 {font} 0 R >>"));
    }
    if let Some(image) = image_object {
        resources.push_str(&format!(" /XObject << /Im1 {image} 0 R >>"));
    }
    resources.push_str(" >>");
    ensure!(pdf.object(resources.as_bytes())? == resources_object);

    if let Some(
        [
            font_object,
            descriptor_object,
            font_stream_object,
            cmap_object,
        ],
    ) = font_objects
    {
        let font = pinned_font(repo_root)?;
        ensure!(
            pdf.object(font_dictionary_with_descriptor(cmap_object, descriptor_object).as_bytes())?
                == font_object
        );
        ensure!(pdf.object(
            format!(
                "<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 {font_stream_object} 0 R >>"
            )
            .as_bytes(),
        )? == descriptor_object);
        ensure!(
            pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)? == font_stream_object
        );
        ensure!(pdf.stream(b"/Type /CMap", operator_walk_to_unicode())? == cmap_object);
    }
    if let Some(image) = image_object {
        let pixels = [
            0x21, 0x54, 0x88, 0xf2, 0xc1, 0x4e, 0xe8, 0x55, 0x55, 0xf4, 0xf1, 0xde,
        ];
        ensure!(pdf.stream(
            b"/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
            &pixels,
        )? == image);
    }
    for (page, object) in pages.iter().zip(content_objects) {
        if let (Some(content), Some(expected_object)) = (page.content(), object) {
            ensure!(pdf.stream(b"", content)? == expected_object);
        }
    }
    ensure!(u32::try_from(pdf.offsets.len())? + 1 == next_object);
    pdf.finish(1)
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

fn basic_text_with_page_size(
    fixture_id: &str,
    repo_root: &Path,
    width: u32,
    height: u32,
    content: &[u8],
) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new(fixture_id);
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources 4 0 R /Contents 9 0 R >>"
        )
        .as_bytes(),
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.stream(b"", content)?;
    pdf.finish(1)
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
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

fn inherited_page_resources(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-parse-07-inherited-page-resources");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 9 0 R >>")?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.stream(b"", STANDARD_CONTENT)?;
    pdf.finish(1)
}

fn indirect_filter(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-parse-indirect-filter");
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    ensure!(pdf.stream(b"/Filter 10 0 R", &ascii_hex(STANDARD_CONTENT))? == 9);
    ensure!(pdf.object(b"/ASCIIHexDecode")? == 10);
    pdf.finish(1)
}

fn midtree_resources(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-parse-midtree-resources");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /MediaBox [0 0 300 200] /Resources 5 0 R >>",
    )?;
    pdf.object(b"<< /Type /Page /Parent 3 0 R /MediaBox [0 0 300 200] /Contents 10 0 R >>")?;
    pdf.object(b"<< /Font << /F1 6 0 R >> >>")?;
    pdf.object(font_dictionary_with_descriptor(9, 7).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 8 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.stream(b"", STANDARD_CONTENT)?;
    pdf.finish(1)
}

fn parse_m1_switchboard(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-parse-m1-switchboard");
    pdf.object(b"<< /Type /Catalog /Pages 02 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [03 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 02 0 R /MediaBox [0 0 300 200] /Resources 04 0 R /Contents 10 0 R /Annots [11 0 R] >>",
    )?;
    pdf.object(b"<< /Font << /F1 05 0 R >> >>")?;
    pdf.object(
        b"<< /Type /Font /Subtype /TrueType /BaseFont /MIMUSI+DejaVuSans /FirstChar 32 /LastChar 85 /Widths [318 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 698 0 632 0 0 0 295 0 0 0 863 0 0 0 0 695 635 611 732] /FontDescriptor 06 0 R /Encoding /WinAnsiEncoding /ToUnicode 08 0 R >>",
    )?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 07 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.object(b"<< /MimusNotAStream true >>")?;
    pdf.stream(b"", STANDARD_CONTENT)?;
    pdf.object(b"<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] >>")?;
    pdf.object(b"<< /MimusFixturePadding true >>")?;
    pdf.object(b"null")?;
    pdf.finish(1)
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
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
    pdf.stream(b"", b"/F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj\n")?;
    pdf.stream(b"", b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS Tj ET\n")?;
    pdf.stream(
        b"",
        b"BT /F1 12 Tf 1 0 0 1 72 120 Tm [(MIM) /X (US)] TJ ET\n",
    )?;
    pdf.stream(
        b"",
        b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MI) Tj BT /F1 12 Tf 1 0 0 1 100 80 Tm (MUS) Tj ET\n",
    )?;
    pdf.stream(b"", b"BT /F1 12 Tf 1 0 0 1 72 120 Tm <4G> Tj ET\n")?;
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
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

fn inline_image_filtered_fallback(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-stream-11-inline-image-filtered-fallback");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 9 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> /Shading << /S1 << /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 300 200] /Function << /FunctionType 2 /Domain [0 1] /C0 [0.95] /C1 [0.85] /N 1 >> /Extend [true true] >> >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.stream(
        b"",
        b"q\nBI /W 8 /H 1 /BPC 8 /CS /G /F /AHx ID\n6060606060606060>\nEI\n/S1 sh\nQ\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n",
    )?;
    pdf.finish(1)
}

fn odd_hex_identity(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut cid_to_gid = vec![0_u8; (144 + 1) * 2];
    for (cid, gid) in [(6_usize, 6_u16), (7, 7), (11, 11), (144, 9)] {
        cid_to_gid[cid * 2..cid * 2 + 2].copy_from_slice(&gid.to_be_bytes());
    }

    let mut pdf = RawPdf::new("unit-stream-odd-hex");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 11 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSI+DejaVuSans /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 10 0 R >>")?;
    pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSI+DejaVuSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /DW 600 /W [6 [295 863] 11 [732] 144 [635]] /CIDToGIDMap 9 0 R >>")?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 8 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"", &cid_to_gid)?;
    pdf.stream(b"/Type /CMap", odd_hex_to_unicode())?;
    pdf.stream(
        b"",
        b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n<000700060007000B009> Tj\nET\n",
    )?;
    pdf.finish(1)
}

fn tr7_clip(repo_root: &Path) -> Result<Vec<u8>> {
    basic_text(
        "unit-stream-tr7-clip",
        repo_root,
        b"q\nBT\n/F1 12 Tf\n1 0 0 1 72 150 Tm\n7 Tr\n(MIMUS) Tj\nET\nQ\nBT\n/F1 12 Tf\n1 0 0 1 72 50 Tm\n0 Tr\n(MIMUS) Tj\nET\n",
    )
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.object(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /FirstChar 65 /LastChar 90 /Widths [1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000] >>")?;
    pdf.stream(b"", b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (AAAA) Tj ET\n")?;
    pdf.finish(1)
}

fn escaped_font_name(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-font-escaped-name");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 8 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(
        b"<< /Type /Font /Subtype /TrueType /BaseFont /MIMUSI+DejaVuSans /FirstChar 73 /LastChar 85 /Widths [295 0 0 0 863 0 0 0 0 0 635 0 732] /FontDescriptor 6 0 R /Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [73 /I 77 /M 83 /S 85 /U] >> >>",
    )?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"", b"BT\n/F#31 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS) Tj\nET\n")?;
    pdf.finish(1)
}

fn embedded_cmap_ok(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-cmap-embedded-ok");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 11 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSI+DejaVuSans /Encoding 9 0 R /DescendantFonts [6 0 R] /ToUnicode 10 0 R >>")?;
    pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSI+DejaVuSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /DW 600 /W [6 [295 863] 9 [635] 11 [732]] /CIDToGIDMap /Identity >>")?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 8 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", embedded_byte_encoding())?;
    pdf.stream(b"/Type /CMap", bfrange_to_unicode())?;
    pdf.stream(
        b"",
        b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n<4D494D5553> Tj\nET\n",
    )?;
    pdf.finish(1)
}

fn identity_cmap_alias(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-cmap-identity-alias");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 9 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSI+DejaVuSans /Encoding /DLIdent-H /DescendantFonts [6 0 R] >>")?;
    pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSI+DejaVuSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /DW 600 /W [6 [295 863] 9 [635] 11 [732]] /CIDToGIDMap /Identity >>")?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 8 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(
        b"",
        b"BT /F1 12 Tf 1 0 0 1 72 120 Tm <000700060007000B0009> Tj ET\n",
    )?;
    pdf.finish(1)
}

fn predefined_gb_cmap(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_cjk_font(repo_root)?;
    let mut cid_to_gid = vec![0_u8; (4559 + 1) * 2];
    for (cid, gid) in [(1193_usize, 10_u16), (3435, 11), (3795, 9), (4559, 8)] {
        cid_to_gid[cid * 2..cid * 2 + 2].copy_from_slice(&gid.to_be_bytes());
    }

    let mut pdf = RawPdf::new("unit-cmap-predefined-gb");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 11 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSC+NotoSansSC-Regular /Encoding /GBK-EUC-H /DescendantFonts [6 0 R] /ToUnicode 10 0 R >>")?;
    pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSC+NotoSansSC-Regular /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> /FontDescriptor 7 0 R /DW 1000 /W [1193 [1000] 3435 [1000] 3795 [1000] 4559 [1000]] /CIDToGIDMap 9 0 R >>")?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSC+NotoSansSC-Regular /Flags 4 /FontBBox [36 -120 967 880] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 733 /StemV 80 /MissingWidth 1000 /FontFile2 8 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"", &cid_to_gid)?;
    pdf.stream(b"/Type /CMap", gbk_to_unicode())?;
    pdf.stream(
        b"",
        b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n<D6D0CEC4B2E2CAD4> Tj\nET\n",
    )?;
    pdf.finish(1)
}

fn mixed_cmap_degradation(repo_root: &Path) -> Result<Vec<u8>> {
    const PAGE_COUNT: u32 = 10;
    const RESOURCES_OBJECT: u32 = 13;
    const FIRST_CONTENT_OBJECT: u32 = 24;

    let latin_font = pinned_font(repo_root)?;
    let cjk_font = pinned_cjk_font(repo_root)?;
    let mut cid_to_gid = vec![0_u8; (4559 + 1) * 2];
    for (cid, gid) in [(1193_usize, 10_u16), (3435, 11), (3795, 9), (4559, 8)] {
        cid_to_gid[cid * 2..cid * 2 + 2].copy_from_slice(&gid.to_be_bytes());
    }

    let mut pdf = RawPdf::new("intg-cmap-mixed-degrade");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    let kids = (0..PAGE_COUNT)
        .map(|index| format!("{} 0 R", index + 3))
        .collect::<Vec<_>>()
        .join(" ");
    pdf.object(format!("<< /Type /Pages /Kids [{kids}] /Count {PAGE_COUNT} >>").as_bytes())?;
    for index in 0..PAGE_COUNT {
        let page_object = pdf.object(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources {RESOURCES_OBJECT} 0 R /Contents {} 0 R >>",
                FIRST_CONTENT_OBJECT + index
            )
            .as_bytes(),
        )?;
        ensure!(page_object == index + 3);
    }

    ensure!(pdf.object(b"<< /Font << /F1 14 0 R /FCJK 18 0 R >> >>")? == RESOURCES_OBJECT);
    ensure!(pdf.object(font_dictionary_with_descriptor(17, 15).as_bytes())? == 14);
    ensure!(
        pdf.object(
            b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 16 0 R >>",
        )? == 15
    );
    ensure!(
        pdf.stream(
            format!("/Length1 {}", latin_font.len()).as_bytes(),
            &latin_font,
        )? == 16
    );
    ensure!(pdf.stream(b"/Type /CMap", to_unicode())? == 17);

    ensure!(
        pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSC+NotoSansSC-Regular /Encoding /GBK-EUC-H /DescendantFonts [19 0 R] /ToUnicode 23 0 R >>")?
            == 18
    );
    ensure!(
        pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSC+NotoSansSC-Regular /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> /FontDescriptor 20 0 R /DW 1000 /W [1193 [1000] 3435 [1000] 3795 [1000] 4559 [1000]] /CIDToGIDMap 22 0 R >>")?
            == 19
    );
    ensure!(
        pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSC+NotoSansSC-Regular /Flags 4 /FontBBox [36 -120 967 880] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 733 /StemV 80 /MissingWidth 1000 /FontFile2 21 0 R >>")?
            == 20
    );
    ensure!(pdf.stream(format!("/Length1 {}", cjk_font.len()).as_bytes(), &cjk_font,)? == 21);
    ensure!(pdf.stream(b"", &cid_to_gid)? == 22);
    ensure!(pdf.stream(b"/Type /CMap", gbk_to_unicode())? == 23);

    for index in 0..PAGE_COUNT {
        let content = if index < 7 {
            STANDARD_CONTENT
        } else {
            b"BT\n/FCJK 12 Tf\n1 0 0 1 72 120 Tm\n<D6D0CEC4B2E2CAD4> Tj\nET\n"
        };
        ensure!(pdf.stream(b"", content)? == FIRST_CONTENT_OBJECT + index);
    }
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSI+DejaVuSans /Encoding /Identity-H /DescendantFonts [10 0 R] >>")?;
    pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSI+DejaVuSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 6 0 R /W [6 [295 863 0 635 0 732]] /CIDToGIDMap /Identity >>")?;
    pdf.stream(
        b"",
        b"BT /F1 12 Tf 1 0 0 1 72 120 Tm <000700060007000B0009> Tj ET\n",
    )?;
    pdf.finish(1)
}

#[derive(Clone, Copy)]
enum AlignmentRecipe {
    IndependentSpace,
    HyphenMarker,
    Ligature,
    Noncharacter,
    DoubleDraw,
    WeakConflict,
    ActualTextOverlap,
    ActualTextDisjoint,
}

impl AlignmentRecipe {
    fn content(self) -> &'static [u8] {
        match self {
            Self::IndependentSpace => {
                b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(A) Tj\n[( )] TJ\nET\n"
            }
            Self::HyphenMarker => {
                b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(M-) Tj\n0 -20 Td\n(M) Tj\nET\n"
            }
            Self::Ligature | Self::Noncharacter => {
                b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(A) Tj\nET\n"
            }
            Self::DoubleDraw => b"BT\n/F1 12 Tf\n1 Tr\n1 0 0 1 72 120 Tm\n(M) Tj\nET\nBT\n/F1 12 Tf\n0 Tr\n1 0 0 1 72 120 Tm\n(A) Tj\nET\n",
            Self::WeakConflict => b"/Span << /ActualText (I) >> BDC\nBT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(M) Tj\nET\nEMC\n",
            Self::ActualTextOverlap => b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(M) Tj\nET\n/Span << /ActualText (MI) >> BDC\nBT\n/F1 12 Tf\n0 1 -1 0 88 120 Tm\n(M) Tj\nET\nEMC\n",
            Self::ActualTextDisjoint => b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n(M) Tj\nET\n/Span << /ActualText (MI) >> BDC\nBT\n/F1 12 Tf\n0 1 -1 0 200 40 Tm\n(M) Tj\nET\nEMC\n",
        }
    }

    fn unicode_mappings(self) -> Option<&'static [(u8, &'static str)]> {
        match self {
            Self::IndependentSpace => Some(&[(0x20, "0020"), (0x41, "0041")]),
            Self::HyphenMarker => Some(&[(0x2d, "002D"), (0x4d, "004D")]),
            Self::Ligature => Some(&[(0x41, "FB01")]),
            Self::Noncharacter => Some(&[(0x41, "FFFF")]),
            Self::DoubleDraw => Some(&[(0x41, "004D"), (0x4d, "004D")]),
            Self::WeakConflict | Self::ActualTextOverlap | Self::ActualTextDisjoint => None,
        }
    }
}

fn alignment_composite_fixture(
    fixture_id: &str,
    repo_root: &Path,
    target: &str,
) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new(fixture_id);
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 11 0 R >>")?;
    pdf.object(b"<< /Font << /F1 9 0 R >> >>")?;
    pdf.object(b"<< /MimusFixturePadding true >>")?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", &alignment_composite_to_unicode(target))?;
    pdf.object(b"<< /Type /Font /Subtype /Type0 /BaseFont /MIMUSI+DejaVuSans /Encoding /Identity-H /DescendantFonts [10 0 R] /ToUnicode 8 0 R >>")?;
    pdf.object(b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MIMUSI+DejaVuSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 6 0 R /W [7 [863]] /CIDToGIDMap /Identity >>")?;
    pdf.stream(b"", b"BT\n/F1 12 Tf\n1 0 0 1 72 120 Tm\n<0007> Tj\nET\n")?;
    pdf.finish(1)
}

fn alignment_composite_to_unicode(target: &str) -> Vec<u8> {
    format!(
        "/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /MimusAlignment-UCS def\n\
/CMapType 2 def\n\
1 begincodespacerange\n\
<0000> <FFFF>\n\
endcodespacerange\n\
1 beginbfchar\n\
<0007> <{target}>\n\
endbfchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end\n"
    )
    .into_bytes()
}

fn alignment_fixture(
    fixture_id: &str,
    repo_root: &Path,
    recipe: AlignmentRecipe,
) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new(fixture_id);
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 9 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    let to_unicode_entry = recipe
        .unicode_mappings()
        .map_or("", |_| " /ToUnicode 8 0 R");
    pdf.object(
        format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /MIMUSI+DejaVuSans \
             /FirstChar 32 /LastChar 77 /Widths [{}] /FontDescriptor 6 0 R \
             /Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding \
             /Differences [45 /M 65 /{}] >>{to_unicode_entry} >>",
            alignment_widths(),
            if matches!(recipe, AlignmentRecipe::Ligature) {
                "fi"
            } else {
                "M"
            }
        )
        .as_bytes(),
    )?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    if let Some(mappings) = recipe.unicode_mappings() {
        pdf.stream(b"/Type /CMap", &alignment_to_unicode(mappings))?;
    } else {
        pdf.object(b"<< /MimusFixturePadding true >>")?;
    }
    pdf.stream(b"", recipe.content())?;
    pdf.finish(1)
}

fn alignment_widths() -> String {
    (32_u8..=77)
        .map(|code| match code {
            32 => 318,
            45 | 65 | 77 => 863,
            73 => 295,
            _ => 0,
        })
        .map(|width| width.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn alignment_to_unicode(mappings: &[(u8, &str)]) -> Vec<u8> {
    let mut cmap = b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /MimusAlignment-UCS def\n\
/CMapType 2 def\n\
1 begincodespacerange\n\
<00> <FF>\n\
endcodespacerange\n"
        .to_vec();
    cmap.extend_from_slice(format!("{} beginbfchar\n", mappings.len()).as_bytes());
    for &(code, target) in mappings {
        cmap.extend_from_slice(format!("<{code:02X}> <{target}>\n").as_bytes());
    }
    cmap.extend_from_slice(
        b"endbfchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end\n",
    );
    cmap
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
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

fn xobject_m1_switchboard(repo_root: &Path) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-xobj-m1-switchboard");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 14 0 R >>")?;
    pdf.object(
        b"<< /Font << /F1 5 0 R >> /XObject << /X0 10 0 R /X2 11 0 R /X3 12 0 R /X4 13 0 R >> >>",
    )?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.object(b"<< /MimusFixturePadding true >>")?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Resources 4 0 R",
        STANDARD_CONTENT,
    )?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 null 200] /Resources 4 0 R",
        STANDARD_CONTENT,
    )?;
    pdf.stream(
        b"/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Matrix [1 0 0 1] /Resources 4 0 R",
        STANDARD_CONTENT,
    )?;
    pdf.object(b"<< /Type /XObject /Subtype /Form /BBox [0 0 300 200] >>")?;
    pdf.stream(b"", b"q /X0 Do Q\n")?;
    pdf.finish(1)
}

fn xobject_depth_overflow(repo_root: &Path) -> Result<Vec<u8>> {
    const FIRST_FORM_OBJECT: u32 = 10;
    const FORM_COUNT: u32 = 65;

    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new("unit-xobj-depth-overflow");
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")?;
    pdf.object(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 4 0 R /Contents 9 0 R >>",
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> /XObject << /X0 10 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>")?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
    pdf.stream(
        b"",
        b"BT /F1 12 Tf 1 0 0 1 72 120 Tm (MIMUS) Tj ET\nq /X0 Do Q\n",
    )?;

    for index in 0..FORM_COUNT {
        let object = FIRST_FORM_OBJECT + index;
        if index + 1 == FORM_COUNT {
            ensure!(
                pdf.stream(
                    b"/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Resources << /Font << /F1 5 0 R >> >>",
                    b"BT /F1 12 Tf 1 0 0 1 72 80 Tm (MIMUS) Tj ET\n",
                )? == object
            );
        } else {
            let next = object + 1;
            let dictionary = format!(
                "/Type /XObject /Subtype /Form /BBox [0 0 300 200] /Resources << /Font << /F1 5 0 R >> /XObject << /Next {next} 0 R >> >>"
            );
            ensure!(pdf.stream(dictionary.as_bytes(), b"/Next Do\n")? == object);
        }
    }
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
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
    pdf.stream(b"/Type /CMap", operator_walk_to_unicode())?;
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

fn ascii_hex(bytes: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = Vec::with_capacity(bytes.len() * 2 + 2);
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)]);
        output.push(HEX[usize::from(byte & 0x0f)]);
    }
    output.extend_from_slice(b">\n");
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

fn geometry_text_page(
    repo_root: &Path,
    fixture_id: &str,
    pages_entries: &str,
    page_entries: &str,
    content: &[u8],
    tail_objects: &[&[u8]],
) -> Result<Vec<u8>> {
    let font = pinned_font(repo_root)?;
    let mut pdf = RawPdf::new(fixture_id);
    pdf.object(b"<< /Type /Catalog /Pages 2 0 R >>")?;
    pdf.object(format!("<< /Type /Pages /Kids [3 0 R] /Count 1{pages_entries} >>").as_bytes())?;
    pdf.object(
        format!("<< /Type /Page /Parent 2 0 R {page_entries} /Resources 4 0 R /Contents 9 0 R >>")
            .as_bytes(),
    )?;
    pdf.object(b"<< /Font << /F1 5 0 R >> >>")?;
    pdf.object(font_dictionary(8).as_bytes())?;
    pdf.object(
        b"<< /Type /FontDescriptor /FontName /MIMUSI+DejaVuSans /Flags 32 /FontBBox [-3 -15 766 743] /ItalicAngle 0 /Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /MissingWidth 600 /FontFile2 7 0 R >>",
    )?;
    pdf.stream(format!("/Length1 {}", font.len()).as_bytes(), &font)?;
    pdf.stream(b"/Type /CMap", to_unicode())?;
    pdf.stream(b"", content)?;
    for object in tail_objects {
        pdf.object(object)?;
    }
    pdf.finish(1)
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
const CJK_FONT_SHA256: &str = "a1677185f15e59c1ccb25e0fb320ab23d3a34d27649496eff089df41e27074ac";

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

fn pinned_cjk_font(repo_root: &Path) -> Result<Vec<u8>> {
    let path = repo_root.join("corpus/fonts/MimusCJK.ttf");
    let bytes = std::fs::read(&path)
        .with_context(|| format!("read pinned CJK fixture font {}", path.display()))?;
    ensure!(
        hash::of_bytes(&bytes) == CJK_FONT_SHA256,
        "pinned CJK fixture font SHA-256 changed: {}",
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

// Experiment 2's admitted byte contracts include the extra characters used by
// its Form and tokenizer fixtures. Keep that CMap separate from experiment 3's
// intentionally minimal MIMUS-only writeback baseline.
fn operator_walk_to_unicode() -> &'static [u8] {
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

fn odd_hex_to_unicode() -> &'static [u8] {
    b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /MimusOddHex-UCS def\n\
/CMapType 2 def\n\
1 begincodespacerange\n\
<0000> <FFFF>\n\
endcodespacerange\n\
4 beginbfchar\n\
<0006> <0049>\n\
<0007> <004D>\n\
<000B> <0055>\n\
<0090> <0053>\n\
endbfchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end\n"
}

fn embedded_byte_encoding() -> &'static [u8] {
    b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def\n\
/CMapName /MimusByte-Encoding def\n\
/CMapType 1 def\n\
1 begincodespacerange\n\
<00> <FF>\n\
endcodespacerange\n\
4 begincidchar\n\
<49> 6\n\
<4D> 7\n\
<53> 9\n\
<55> 11\n\
endcidchar\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end\n"
}

fn bfrange_to_unicode() -> &'static [u8] {
    b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /MimusRange-UCS def\n\
/CMapType 2 def\n\
1 begincodespacerange\n\
<00> <FF>\n\
endcodespacerange\n\
3 beginbfrange\n\
<49> <49> <0049>\n\
<4D> <4D> <004D>\n\
<53> <55> <0053>\n\
endbfrange\n\
endcmap\n\
CMapName currentdict /CMap defineresource pop\n\
end\n\
end\n"
}

fn gbk_to_unicode() -> &'static [u8] {
    b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\n\
begincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /MimusGBK-UCS def\n\
/CMapType 2 def\n\
1 begincodespacerange\n\
<8140> <FEFE>\n\
endcodespacerange\n\
4 beginbfchar\n\
<D6D0> <4E2D>\n\
<CEC4> <6587>\n\
<B2E2> <6D4B>\n\
<CAD4> <8BD5>\n\
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
    fn scan_recipes_support_fontless_images_and_deterministic_multi_page_order() {
        let image = generate("unit-scan-01-image-only", &repo_root()).unwrap();
        assert!(contains(&image, b"/Subtype /Image"));
        assert!(contains(&image, b"/Im1 Do"));
        assert!(!contains(&image, b"/Type /Font"));

        let first = generate("intg-scan-08-text-first", &repo_root()).unwrap();
        let second = generate("intg-scan-08-text-first", &repo_root()).unwrap();
        assert_eq!(first, second);
        assert!(contains(&first, b"/Count 4"));
        assert!(contains(&first, b"/Kids [3 0 R 4 0 R 5 0 R 6 0 R]"));
    }

    #[test]
    fn blank_scan_pages_omit_contents_instead_of_using_empty_streams() {
        let bytes = generate("intg-scan-12-image-with-blank-backs", &repo_root()).unwrap();
        assert_eq!(
            bytes
                .windows(b"/Contents".len())
                .filter(|window| *window == b"/Contents")
                .count(),
            1
        );
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
