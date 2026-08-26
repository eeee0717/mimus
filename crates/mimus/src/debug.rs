use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use mimus_core::PassSnapshotSink;
use mimus_core::error::{IoReason, MimusError, Result};
use mimus_core::event::{DiagnosticEvent, Event, EventKind, Stage, serialize_line};
use mimus_core::il;

#[derive(Debug)]
pub(crate) struct DebugArtifacts {
    directory: PathBuf,
}

impl DebugArtifacts {
    pub(crate) fn create(directory: PathBuf) -> Result<Self> {
        std::fs::create_dir(&directory).map_err(|error| {
            debug_write_error(format!(
                "could not create debug directory {}: {error}",
                directory.display()
            ))
        })?;
        Ok(Self { directory })
    }

    pub(crate) fn write_diagnostics(&self, diagnostics: &[DiagnosticEvent]) -> Result<()> {
        let mut contents = Vec::new();
        for diagnostic in diagnostics {
            contents.extend_from_slice(&serialize_line(&Event::new(EventKind::Diagnostic {
                diagnostic: diagnostic.clone(),
            }))?);
        }
        self.write_atomic("diagnostics.ndjson", &contents)
    }

    fn write_atomic(&self, filename: &str, contents: &[u8]) -> Result<()> {
        let destination = self.directory.join(filename);
        let mut temporary = tempfile::Builder::new()
            .prefix(".mimus-debug-")
            .suffix(".tmp")
            .tempfile_in(&self.directory)
            .map_err(|error| debug_write_error(format!("could not create debug file: {error}")))?;
        write_debug_file(temporary.as_file_mut(), contents)?;
        temporary.persist_noclobber(&destination).map_err(|error| {
            debug_write_error(format!(
                "could not atomically publish {}: {}",
                destination.display(),
                error.error
            ))
        })?;
        Ok(())
    }
}

impl PassSnapshotSink for DebugArtifacts {
    fn write_snapshot(
        &self,
        pass_index: usize,
        stage: Stage,
        snapshot: &il::Document,
    ) -> Result<()> {
        let filename = format!("{pass_index:02}-{}.il.json", stage.wire_name());
        let contents = il::canonical_json(snapshot)?;
        self.write_atomic(&filename, &contents)
    }
}

fn write_debug_file(file: &mut File, contents: &[u8]) -> Result<()> {
    file.write_all(contents)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|error| debug_write_error(format!("could not write debug file: {error}")))
}

fn debug_write_error(message: impl Into<String>) -> MimusError {
    MimusError::io(IoReason::DebugWrite, message)
}

#[cfg(test)]
mod tests {
    use mimus_core::error::ErrorReason;
    use mimus_core::event::{DiagnosticId, DroppedDiagnosticCount};

    use super::*;

    fn filenames(directory: &std::path::Path) -> Vec<String> {
        let mut names = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .into_string()
                    .expect("debug filenames are UTF-8")
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn create_rejects_an_existing_directory_without_modifying_it() {
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("debug");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("sentinel"), b"keep").unwrap();

        let error = DebugArtifacts::create(directory.clone()).unwrap_err();

        assert_eq!(error.reason(), ErrorReason::Io(IoReason::DebugWrite));
        assert_eq!(std::fs::read(directory.join("sentinel")).unwrap(), b"keep");
        assert_eq!(filenames(&directory), vec!["sentinel"]);
    }

    #[test]
    fn snapshots_use_stage_names_canonical_bytes_and_leave_no_temporary_files() {
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("debug");
        let debug = DebugArtifacts::create(directory.clone()).unwrap();
        let snapshot = il::Document::default();

        debug
            .write_snapshot(3, Stage::ParagraphFind, &snapshot)
            .unwrap();

        let path = directory.join("03-paragraph_find.il.json");
        assert_eq!(
            std::fs::read(path).unwrap(),
            il::canonical_json(&snapshot).unwrap()
        );
        assert_eq!(filenames(&directory), vec!["03-paragraph_find.il.json"]);
    }

    #[test]
    fn diagnostics_file_uses_the_shared_v2_event_serializer() {
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("debug");
        let debug = DebugArtifacts::create(directory.clone()).unwrap();
        let diagnostics = vec![
            DiagnosticEvent::EngineBaselineMismatch {
                page_index: 0,
                character_index: 2,
                delta_x_pt: 0.01,
                delta_y_pt: 0.02,
            },
            DiagnosticEvent::DroppedDiagnostics {
                count: 7,
                counts_by_id: vec![DroppedDiagnosticCount {
                    id: DiagnosticId::EngineBaselineMismatch,
                    count: 7,
                }],
            },
        ];

        debug.write_diagnostics(&diagnostics).unwrap();

        let expected = diagnostics
            .iter()
            .flat_map(|diagnostic| {
                serialize_line(&Event::new(EventKind::Diagnostic {
                    diagnostic: diagnostic.clone(),
                }))
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            std::fs::read(directory.join("diagnostics.ndjson")).unwrap(),
            expected
        );
        assert_eq!(filenames(&directory), vec!["diagnostics.ndjson"]);
    }

    #[test]
    fn empty_diagnostics_still_creates_an_empty_file() {
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("debug");
        let debug = DebugArtifacts::create(directory.clone()).unwrap();

        debug.write_diagnostics(&[]).unwrap();

        assert_eq!(
            std::fs::read(directory.join("diagnostics.ndjson")).unwrap(),
            b""
        );
        assert_eq!(filenames(&directory), vec!["diagnostics.ndjson"]);
    }
}
