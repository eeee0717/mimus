use std::{fs, process::Command};

#[test]
fn help_is_available_without_production_crates() {
    let output = Command::new(env!("CARGO_BIN_EXE_scorecard"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Measure")
    );
}

#[test]
fn manifest_does_not_depend_on_production_crates() {
    let manifest = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(!manifest.contains("mimus-core"));
    assert!(!manifest.contains("pdfium-render"));
    assert!(!manifest.contains("lopdf"));
}
