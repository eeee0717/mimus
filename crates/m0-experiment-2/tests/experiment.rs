use std::collections::BTreeSet;
use std::path::PathBuf;

use approx::assert_abs_diff_eq;
use m0_experiment_2::run_fixture;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn base_fixture_matches_the_hand_written_character_trace() {
    let report = run_fixture(&repo_root(), "unit-base-01-single-line", None).unwrap();

    assert_eq!(
        report
            .glyphs
            .iter()
            .filter_map(|glyph| glyph.unicode)
            .collect::<String>(),
        "MIMUS"
    );
    assert_abs_diff_eq!(report.glyphs[0].baseline[0], 72.0, epsilon = 0.001);
    assert_abs_diff_eq!(report.glyphs[0].baseline[1], 120.0, epsilon = 0.001);
    assert!(report.errors.is_empty());
}

#[test]
fn every_legal_experiment_fixture_matches_its_manifest_trace() {
    let fixtures = [
        "unit-parse-01-ascii85",
        "unit-parse-02-cascade",
        "unit-parse-03-lzw-earlychange",
        "unit-parse-03-lzw-earlychange-1",
        "unit-parse-04-contents-array-numeric-split",
        "unit-parse-07-inherited-page-resources",
        "unit-stream-01-bx-ex-unknown-op",
        "unit-stream-02-type3-d1",
        "unit-stream-03-unknown-op-outside-bx",
        "unit-stream-04-type3-d0",
        "unit-stream-08-inline-image-EI-in-data",
        "unit-stream-09-inline-image-no-L",
        "unit-stream-10-inline-image-length",
        "unit-stream-11-inline-image-filtered-fallback",
        "unit-font-01-std14-custom-widths",
        "unit-cmap-01-identity-no-tounicode",
        "unit-xobj-00-recursion-parent",
        "unit-xobj-04-inherited-resources",
        "unit-xobj-05-scope-parent",
    ];

    for fixture in fixtures {
        let report = run_fixture(&repo_root(), fixture, None).unwrap();
        assert!(report.errors.is_empty(), "{fixture}: {:?}", report.errors);
        assert!(
            report.manifest.text_matches,
            "{fixture}: {:?}",
            report.manifest
        );
        assert!(report.manifest.cid_sequence_matches, "{fixture}");
        let delta = report.manifest.baseline_delta.unwrap();
        assert_abs_diff_eq!(delta[0], 0.0, epsilon = report.manifest.tolerance_pt);
        assert_abs_diff_eq!(delta[1], 0.0, epsilon = report.manifest.tolerance_pt);
    }
}

#[test]
fn malformed_experiment_fixtures_finish_with_the_declared_diagnostic() {
    let fixtures = [
        (
            "mal-parse-05-contents-array-string-split",
            "unterminated-string",
        ),
        ("mal-parse-06-deep-nesting", "nesting-too-deep-128"),
        ("mal-parse-07-parent-cycle", "page-tree-cycle"),
        ("mal-stream-03-arity-excess", "arity-excess"),
        ("mal-stream-04-arity-short", "arity-short"),
        ("mal-stream-05-unbalanced-Q", "graphics-stack-underflow"),
        ("mal-stream-06-glued-tokens", "glued-token-recovery"),
        (
            "mal-stream-07-double-decimal",
            "double-decimal,arity-excess",
        ),
        (
            "mal-stream-08-unknown-outside-bx",
            "unknown-operator,compatibility-underflow",
        ),
        ("mal-xobj-01-self-recursive", "recursive-form-self"),
        ("mal-xobj-02-mutual-recursive", "recursive-form-mutual"),
        ("mal-xobj-03-form-no-bbox", "form-missing-bbox"),
        ("mal-xobj-04-scope-underflow", "graphics-stack-underflow"),
        (
            "mal-xobj-05-scope-tail",
            "scoped-graphics-stack-unbalanced,scoped-operands-discarded",
        ),
    ];

    for (fixture, expected_ids) in fixtures {
        let report = run_fixture(&repo_root(), fixture, None).unwrap();
        let diagnostics = report
            .warnings
            .iter()
            .chain(&report.errors)
            .map(|diagnostic| diagnostic.id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            diagnostics,
            expected_ids.split(',').collect::<BTreeSet<_>>(),
            "{fixture}"
        );
        assert!(report.manifest.diagnostic_matches, "{fixture}");
        if fixture == "mal-parse-07-parent-cycle" {
            assert!(report.streams.is_empty());
            assert!(report.operators.is_empty());
            assert!(report.glyphs.is_empty());
        }
        if fixture.starts_with("mal-xobj-0") && fixture.contains("scope-") {
            let delta = report.manifest.baseline_delta.expect(fixture);
            assert_abs_diff_eq!(delta[0], 0.0, epsilon = report.manifest.tolerance_pt);
            assert_abs_diff_eq!(delta[1], 0.0, epsilon = report.manifest.tolerance_pt);
        }
    }
}

#[test]
fn pdfium_character_origins_cross_check_the_walk() {
    let library = std::env::var_os("MIMUS_PDFIUM_LIBRARY")
        .map(PathBuf::from)
        .expect("MIMUS_PDFIUM_LIBRARY must be set; the PDFium cross-check is mandatory");
    assert!(
        library.is_file(),
        "MIMUS_PDFIUM_LIBRARY does not name a file: {}",
        library.display()
    );

    let fixtures = [
        "unit-base-01-single-line",
        "unit-parse-01-ascii85",
        "unit-parse-02-cascade",
        "unit-parse-03-lzw-earlychange",
        "unit-parse-03-lzw-earlychange-1",
        "unit-parse-04-contents-array-numeric-split",
        "unit-parse-07-inherited-page-resources",
        "unit-stream-01-bx-ex-unknown-op",
        "unit-stream-02-type3-d1",
        "unit-stream-03-unknown-op-outside-bx",
        "unit-stream-04-type3-d0",
        "unit-stream-08-inline-image-EI-in-data",
        "unit-stream-09-inline-image-no-L",
        "unit-stream-10-inline-image-length",
        "unit-stream-11-inline-image-filtered-fallback",
        "unit-font-01-std14-custom-widths",
        "unit-cmap-01-identity-no-tounicode",
        "unit-xobj-00-recursion-parent",
        "unit-xobj-04-inherited-resources",
        "unit-xobj-05-scope-parent",
    ];
    for fixture in fixtures {
        let report = run_fixture(&repo_root(), fixture, Some(&library)).unwrap();
        let pdfium = report.pdfium.as_ref().unwrap();
        let pdfium_chars = pdfium
            .characters
            .iter()
            .filter(|character| character.unicode.is_some_and(|value| !value.is_control()))
            .collect::<Vec<_>>();
        let pdfium_text = pdfium_chars
            .iter()
            .filter_map(|character| character.unicode)
            .collect::<String>();
        if fixture == "unit-cmap-01-identity-no-tounicode" {
            assert!(
                pdfium_text.is_empty(),
                "CMAP-04 PDFium differential changed"
            );
            assert_eq!(report.manifest.observed_text, "MIMUS");
            continue;
        }
        assert_eq!(pdfium_text, report.manifest.expected_text, "{fixture}");
        assert_eq!(pdfium_chars.len(), report.glyphs.len(), "{fixture}");
        for (pdfium, walked) in pdfium_chars.into_iter().zip(&report.glyphs) {
            assert_abs_diff_eq!(
                pdfium.origin[0],
                walked.baseline[0],
                epsilon = report.manifest.tolerance_pt
            );
            assert_abs_diff_eq!(
                pdfium.origin[1],
                walked.baseline[1],
                epsilon = report.manifest.tolerance_pt
            );
        }
    }
}

#[test]
fn raw_operator_trace_preserves_type3_inline_image_and_unknown_bytes() {
    let type3 = run_fixture(&repo_root(), "unit-stream-02-type3-d1", None).unwrap();
    let d1 = type3
        .operators
        .iter()
        .find(|operator| operator.operator == "d1")
        .expect("Type3 CharProc d1 must be walked");
    assert_eq!(d1.operands, ["1000", "0", "0", "0", "1000", "1000"]);
    let metrics = type3.glyphs[0]
        .type3_metrics
        .as_ref()
        .expect("d1 metrics must be attached to the shown glyph");
    assert_eq!(metrics.width, [1000.0, 0.0]);
    assert_eq!(metrics.bbox, Some([0.0, 0.0, 1000.0, 1000.0]));

    let type3_d0 = run_fixture(&repo_root(), "unit-stream-04-type3-d0", None).unwrap();
    let d0_metrics = type3_d0.glyphs[0]
        .type3_metrics
        .as_ref()
        .expect("d0 metrics must be attached to the shown glyph");
    assert_eq!(d0_metrics.width, [1000.0, 0.0]);
    assert_eq!(d0_metrics.bbox, None);

    let inline = run_fixture(&repo_root(), "unit-stream-08-inline-image-EI-in-data", None).unwrap();
    let image = inline
        .operators
        .iter()
        .find(|operator| operator.operator == "BI..EI")
        .expect("computed-length inline image must be one token");
    assert_eq!(image.inline_image_payload_bytes, Some(4));
    assert_eq!(
        image.inline_image_length_source.as_deref(),
        Some("computed")
    );
    assert!(
        image.raw_hex.contains("20454920"),
        "false EI bytes must stay in payload"
    );

    let declared = run_fixture(&repo_root(), "unit-stream-10-inline-image-length", None).unwrap();
    let declared_image = declared
        .operators
        .iter()
        .find(|operator| operator.operator == "BI..EI")
        .unwrap();
    assert_eq!(declared_image.inline_image_payload_bytes, Some(8));
    assert_eq!(
        declared_image.inline_image_length_source.as_deref(),
        Some("declared")
    );

    let fallback = run_fixture(
        &repo_root(),
        "unit-stream-11-inline-image-filtered-fallback",
        None,
    )
    .unwrap();
    let fallback_image = fallback
        .operators
        .iter()
        .find(|operator| operator.operator == "BI..EI")
        .unwrap();
    assert_eq!(fallback_image.inline_image_payload_bytes, Some(17));
    assert_eq!(
        fallback_image.inline_image_length_source.as_deref(),
        Some("ei-scan")
    );
    assert_eq!(
        fallback
            .warnings
            .iter()
            .map(|warning| warning.id.as_str())
            .collect::<Vec<_>>(),
        ["inline-image-ei-scan"]
    );

    let unknown = run_fixture(&repo_root(), "unit-stream-01-bx-ex-unknown-op", None).unwrap();
    let vendor = unknown
        .operators
        .iter()
        .find(|operator| operator.operator == "SomeVendorOp")
        .expect("unknown operator must remain in the trace");
    assert_eq!(vendor.raw_hex, "536f6d6556656e646f724f70");
    assert!(unknown.warnings.is_empty());

    let outside = run_fixture(&repo_root(), "mal-stream-08-unknown-outside-bx", None).unwrap();
    assert_eq!(
        outside
            .warnings
            .iter()
            .map(|warning| warning.id.as_str())
            .collect::<Vec<_>>(),
        ["unknown-operator", "compatibility-underflow"]
    );
    assert!(outside.manifest.diagnostic_matches);
}

#[test]
fn nested_form_resources_restore_page_font_scope() {
    let report = run_fixture(&repo_root(), "unit-xobj-04-inherited-resources", None).unwrap();
    assert_eq!(report.manifest.observed_text, "IIIIIIH");
    assert_abs_diff_eq!(
        report.glyphs[1].baseline[0] - report.glyphs[0].baseline[0],
        7.2,
        epsilon = 0.001
    );
    assert_abs_diff_eq!(
        report.glyphs[4].baseline[0] - report.glyphs[3].baseline[0],
        3.336,
        epsilon = 0.001
    );
    assert_abs_diff_eq!(report.glyphs[0].baseline[0], 110.0, epsilon = 0.001);
    assert_abs_diff_eq!(report.glyphs[0].baseline[1], 176.0, epsilon = 0.001);
    assert_abs_diff_eq!(report.glyphs[3].baseline[0], 72.0, epsilon = 0.001);
    assert_abs_diff_eq!(report.glyphs[3].baseline[1], 80.0, epsilon = 0.001);
}

#[test]
fn operator_trace_contract_covers_recovery_counts_and_key_ctms() {
    let glued = run_fixture(&repo_root(), "mal-stream-06-glued-tokens", None).unwrap();
    assert_eq!(glued.operators.len(), 5);
    assert_eq!(
        glued
            .operators
            .iter()
            .map(|operator| operator.operator.as_str())
            .collect::<Vec<_>>(),
        ["BT", "Tf", "Td", "Tj", "ET"]
    );
    let tf = &glued.operators[1];
    assert_eq!(tf.operands, ["/F1", "12"]);
    assert_eq!(tf.raw_hex, "31325466");
    let td = &glued.operators[2];
    assert_eq!(td.operands, ["100", "120"]);
    assert_eq!(td.raw_hex, "3132305464");

    let unbalanced = run_fixture(&repo_root(), "mal-stream-05-unbalanced-Q", None).unwrap();
    let restores = unbalanced
        .operators
        .iter()
        .filter(|operator| operator.operator == "Q")
        .collect::<Vec<_>>();
    assert_eq!(restores.len(), 2);
    for restore in restores {
        assert_eq!(restore.ctm_before.0, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(restore.ctm_after.0, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    let form = run_fixture(&repo_root(), "unit-xobj-04-inherited-resources", None).unwrap();
    let page_cm = form
        .operators
        .iter()
        .find(|operator| {
            operator.operator == "cm" && operator.operands == ["1", "0", "0", "1", "10", "15"]
        })
        .unwrap();
    assert_eq!(page_cm.ctm_before.0, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    assert_eq!(page_cm.ctm_after.0, [1.0, 0.0, 0.0, 1.0, 10.0, 15.0]);
    let nested_cm = form
        .operators
        .iter()
        .find(|operator| {
            operator.operator == "cm" && operator.operands == ["1", "0", "0", "1", "3", "4"]
        })
        .unwrap();
    assert_eq!(nested_cm.ctm_before.0, [1.0, 0.0, 0.0, 1.0, 30.0, 45.0]);
    assert_eq!(nested_cm.ctm_after.0, [1.0, 0.0, 0.0, 1.0, 33.0, 49.0]);

    let inline = run_fixture(
        &repo_root(),
        "unit-stream-11-inline-image-filtered-fallback",
        None,
    )
    .unwrap();
    assert_eq!(
        inline
            .operators
            .iter()
            .map(|operator| operator.operator.as_str())
            .collect::<Vec<_>>(),
        ["q", "BI..EI", "sh", "Q", "BT", "Tf", "Tm", "Tj", "ET"]
    );
    let image = &inline.operators[1];
    assert_eq!(image.inline_image_payload_bytes, Some(17));
    assert_eq!(image.inline_image_length_source.as_deref(), Some("ei-scan"));
}

#[test]
fn lzw_early_change_variants_cross_the_code_width_boundary() {
    let zero = run_fixture(&repo_root(), "unit-parse-03-lzw-earlychange", None).unwrap();
    let one = run_fixture(&repo_root(), "unit-parse-03-lzw-earlychange-1", None).unwrap();

    assert_ne!(zero.streams[0].raw_hex, one.streams[0].raw_hex);
    assert_eq!(zero.streams[0].decoded_hex, one.streams[0].decoded_hex);
    assert!(zero.manifest.text_matches);
    assert!(one.manifest.text_matches);
}
