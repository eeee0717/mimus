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
        "unit-parse-04-contents-array-numeric-split",
        "unit-stream-01-bx-ex-unknown-op",
        "unit-stream-02-type3-d1",
        "unit-stream-08-inline-image-EI-in-data",
        "unit-stream-09-inline-image-no-L",
        "unit-font-01-std14-custom-widths",
        "unit-cmap-01-identity-no-tounicode",
        "unit-xobj-00-recursion-parent",
        "unit-xobj-04-inherited-resources",
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
        ("mal-stream-03-arity-excess", "arity-excess"),
        ("mal-stream-04-arity-short", "arity-short"),
        ("mal-stream-05-unbalanced-Q", "graphics-stack-underflow"),
        ("mal-stream-06-glued-tokens", "glued-token-recovery"),
        ("mal-stream-07-double-decimal", "double-decimal"),
        ("mal-xobj-01-self-recursive", "recursive-form-self"),
        ("mal-xobj-02-mutual-recursive", "recursive-form-mutual"),
        ("mal-xobj-03-form-no-bbox", "form-missing-bbox"),
    ];

    for (fixture, expected_id) in fixtures {
        let report = run_fixture(&repo_root(), fixture, None).unwrap();
        let diagnostics = report
            .warnings
            .iter()
            .chain(&report.errors)
            .map(|diagnostic| diagnostic.id.as_str())
            .collect::<Vec<_>>();
        assert!(
            diagnostics.contains(&expected_id),
            "{fixture}: expected {expected_id}, got {diagnostics:?}"
        );
        assert!(report.manifest.diagnostic_matches, "{fixture}");
    }
}

#[test]
fn pdfium_character_origins_cross_check_the_walk() {
    let Some(library) = std::env::var_os("MIMUS_PDFIUM_LIBRARY").map(PathBuf::from) else {
        eprintln!("skipping local PDFium cross-check; MIMUS_PDFIUM_LIBRARY is unset");
        return;
    };
    if !library.is_file() {
        eprintln!(
            "skipping local PDFium cross-check; {} is absent",
            library.display()
        );
        return;
    }

    let fixtures = [
        "unit-base-01-single-line",
        "unit-parse-01-ascii85",
        "unit-parse-02-cascade",
        "unit-parse-03-lzw-earlychange",
        "unit-parse-04-contents-array-numeric-split",
        "unit-stream-01-bx-ex-unknown-op",
        "unit-stream-02-type3-d1",
        "unit-stream-08-inline-image-EI-in-data",
        "unit-stream-09-inline-image-no-L",
        "unit-font-01-std14-custom-widths",
        "unit-cmap-01-identity-no-tounicode",
        "unit-xobj-00-recursion-parent",
        "unit-xobj-04-inherited-resources",
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

    let inline = run_fixture(&repo_root(), "unit-stream-08-inline-image-EI-in-data", None).unwrap();
    let image = inline
        .operators
        .iter()
        .find(|operator| operator.operator == "BI..EI")
        .expect("computed-length inline image must be one token");
    assert!(
        image.raw_hex.contains("20454920"),
        "false EI bytes must stay in payload"
    );

    let unknown = run_fixture(&repo_root(), "unit-stream-01-bx-ex-unknown-op", None).unwrap();
    let vendor = unknown
        .operators
        .iter()
        .find(|operator| operator.operator == "SomeVendorOp")
        .expect("unknown operator must remain in the trace");
    assert_eq!(vendor.raw_hex, "536f6d6556656e646f724f70");
}
