use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use mimus_core::PassSnapshotSink;
use mimus_core::error::{IoReason, MimusError, Result};
use mimus_core::event::{DiagnosticEvent, Event, EventKind, Stage, serialize_line};
use mimus_core::il;

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
