use std::path::{Path, PathBuf};

use mimus_core::error::{AssetReason, MimusError, Result};
use mimus_core::event::EventSink;
use sha2::{Digest, Sha256};

use crate::assets::{self, AssetId};
use crate::config::LayoutModelPathSelection;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedLayoutModel {
    pub path: PathBuf,
    pub source: String,
    pub sha256: String,
    pub bytes: u64,
}

pub(crate) fn resolve_layout_model(
    explicit: Option<&LayoutModelPathSelection>,
    cache_root: &Path,
    mirror: Option<&str>,
    events: &dyn EventSink,
) -> Result<ResolvedLayoutModel> {
    let descriptor = assets::descriptor(AssetId::PpDocLayoutV3);
    if let Some(selection) = explicit {
        let bytes = read_model(&selection.path)?;
        let actual_sha256 = sha256(&bytes);
        validate_sha256(&actual_sha256, descriptor.sha256)?;
        return Ok(ResolvedLayoutModel {
            path: selection.path.clone(),
            source: format!("{}:{}", selection.source, selection.path.display()),
            sha256: actual_sha256,
            bytes: bytes.len() as u64,
        });
    }

    let resolved =
        assets::resolve_managed_asset(descriptor, cache_root, mirror, events, |bytes| {
            if bytes.is_empty() {
                return Err(missing_model_error("layout model is empty"));
            }
            Ok(())
        })?;
    Ok(ResolvedLayoutModel {
        path: resolved.path,
        source: resolved.source,
        sha256: resolved.sha256,
        bytes: resolved.bytes.len() as u64,
    })
}

fn read_model(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).map_err(|error| {
        missing_model_error(format!(
            "could not read layout model {}: {error}",
            path.display()
        ))
    })?;
    let max_bytes = assets::descriptor(AssetId::PpDocLayoutV3).max_bytes;
    if bytes.is_empty() || bytes.len() as u64 > max_bytes {
        return Err(missing_model_error("layout model size is invalid"));
    }
    Ok(bytes)
}

fn validate_sha256(actual: &str, expected: &str) -> Result<()> {
    if actual == expected {
        return Ok(());
    }
    Err(missing_model_error(format!(
        "layout model failed SHA-256 validation: expected {expected}, got {actual}"
    )))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn missing_model_error(message: impl Into<String>) -> MimusError {
    MimusError::asset(AssetReason::LayoutModelUnavailable, message)
        .with_hint("provide the pinned model with --layout-model or check the asset mirror")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mimus_core::event::NoopEventSink;

    use super::*;

    #[test]
    fn manifest_model_identity_matches_the_public_constants() {
        let descriptor = assets::descriptor(AssetId::PpDocLayoutV3);
        assert!(
            descriptor
                .url
                .contains("46bbdf188bb0a772c08aed74882ce7e51a8f1ea6")
        );
        assert_eq!(
            descriptor.sha256,
            "45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba"
        );
        assert!(descriptor.cache_path.ends_with("/inference.onnx"));
    }

    #[test]
    fn explicit_model_path_wins_without_downloading() {
        let Some(path) = std::env::var_os("MIMUS_PINNED_LAYOUT_MODEL").map(PathBuf::from) else {
            return;
        };
        let selection = LayoutModelPathSelection {
            path: path.clone(),
            source: "test",
        };
        let resolved = resolve_layout_model(
            Some(&selection),
            Path::new("unused"),
            Some("http://127.0.0.1:9"),
            &NoopEventSink,
        )
        .unwrap();
        assert_eq!(resolved.path, path);
        assert!(resolved.source.starts_with("test:"));
        assert_eq!(resolved.sha256, descriptor_sha256());
    }

    #[test]
    fn unreadable_explicit_model_is_an_asset_exit_three_error() {
        let selection = LayoutModelPathSelection {
            path: PathBuf::from("missing-model.onnx"),
            source: "test",
        };
        let error =
            resolve_layout_model(Some(&selection), Path::new("unused"), None, &NoopEventSink)
                .unwrap_err();
        assert_eq!(error.category().code(), 3);
    }

    fn descriptor_sha256() -> &'static str {
        assets::descriptor(AssetId::PpDocLayoutV3).sha256
    }
}
