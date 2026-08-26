use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mimus_core::error::{AssetReason, MimusError, Result};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

use crate::config::LayoutModelPathSelection;

pub(crate) const MODEL_COMMIT: &str = "46bbdf188bb0a772c08aed74882ce7e51a8f1ea6";
pub(crate) const MODEL_SHA256: &str =
    "45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba";
const MAX_MODEL_BYTES: u64 = 160 * 1024 * 1024;

#[derive(Debug, Clone)]
struct ModelDescriptor {
    filename: String,
    url: String,
    sha256: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedLayoutModel {
    pub path: PathBuf,
    pub source: String,
    pub sha256: String,
}

pub(crate) fn resolve_layout_model(
    explicit: Option<&LayoutModelPathSelection>,
    cache_dir: &Path,
    mirror: Option<&str>,
) -> Result<ResolvedLayoutModel> {
    resolve_with_manifest(explicit, cache_dir, mirror, &production_manifest())
}

fn production_manifest() -> ModelDescriptor {
    ModelDescriptor {
        filename: "inference.onnx".to_owned(),
        url: format!(
            "https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx/resolve/{MODEL_COMMIT}/inference.onnx"
        ),
        sha256: MODEL_SHA256.to_owned(),
    }
}

fn resolve_with_manifest(
    explicit: Option<&LayoutModelPathSelection>,
    cache_dir: &Path,
    mirror: Option<&str>,
    manifest: &ModelDescriptor,
) -> Result<ResolvedLayoutModel> {
    if let Some(selection) = explicit {
        let bytes = read_model(&selection.path)?;
        let actual_sha256 = sha256(&bytes);
        validate_sha256(&actual_sha256, manifest)?;
        return Ok(ResolvedLayoutModel {
            path: selection.path.clone(),
            source: format!("{}:{}", selection.source, selection.path.display()),
            sha256: actual_sha256,
        });
    }

    let cache_path = cache_dir.join(&manifest.filename);
    if let Ok(bytes) = read_model(&cache_path)
        && sha256(&bytes) == manifest.sha256
    {
        return Ok(ResolvedLayoutModel {
            path: cache_path.clone(),
            source: format!("cache:{}", cache_path.display()),
            sha256: manifest.sha256.clone(),
        });
    }

    let url = asset_url(mirror, manifest)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|_| missing_model_error())?;
    let bytes = download(&client, &url)?;
    let actual_sha256 = sha256(&bytes);
    validate_sha256(&actual_sha256, manifest)?;
    publish_cache(&cache_path, &bytes)?;
    Ok(ResolvedLayoutModel {
        path: cache_path,
        source: format!("download:{url}"),
        sha256: actual_sha256,
    })
}

fn read_model(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).map_err(|_| missing_model_error())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_MODEL_BYTES {
        return Err(missing_model_error());
    }
    Ok(bytes)
}

fn asset_url(mirror: Option<&str>, descriptor: &ModelDescriptor) -> Result<String> {
    let value = mirror.map_or_else(
        || descriptor.url.clone(),
        |base| format!("{}/{}", base.trim_end_matches('/'), descriptor.filename),
    );
    let url = reqwest::Url::parse(&value).map_err(|_| missing_model_error())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(MimusError::asset(
            AssetReason::LayoutModelUnavailable,
            "asset mirror must be an http(s) base URL without credentials, query, or fragment",
        ));
    }
    Ok(value)
}

fn download(client: &Client, url: &str) -> Result<Vec<u8>> {
    let response = client.get(url).send().map_err(|_| missing_model_error())?;
    if !response.status().is_success() {
        return Err(missing_model_error());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_MODEL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| missing_model_error())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_MODEL_BYTES {
        return Err(missing_model_error());
    }
    Ok(bytes)
}

fn publish_cache(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().ok_or_else(missing_model_error)?;
    std::fs::create_dir_all(directory).map_err(|_| missing_model_error())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(directory).map_err(|_| missing_model_error())?;
    std::io::Write::write_all(&mut temporary, bytes).map_err(|_| missing_model_error())?;
    temporary.persist(path).map_err(|_| missing_model_error())?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_sha256(actual: &str, manifest: &ModelDescriptor) -> Result<()> {
    if actual == manifest.sha256 {
        return Ok(());
    }
    Err(MimusError::asset(
        AssetReason::LayoutModelUnavailable,
        format!(
            "layout model failed SHA-256 validation: expected {}, got {actual}",
            manifest.sha256
        ),
    )
    .with_hint("provide the pinned model with --layout-model / MIMUS_LAYOUT_MODEL or check the asset mirror"))
}

fn missing_model_error() -> MimusError {
    MimusError::asset(
        AssetReason::LayoutModelUnavailable,
        "PP-DocLayoutV3 model could not be resolved",
    )
    .with_hint("provide --layout-model / MIMUS_LAYOUT_MODEL or configure MIMUS_ASSET_MIRROR")
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::thread;

    use mimus_core::error::{AssetReason, ErrorReason};
    use sha2::Sha256;

    use super::*;

    #[test]
    fn production_manifest_is_commit_and_sha_pinned() {
        let manifest = production_manifest();
        assert!(manifest.url.contains(MODEL_COMMIT));
        assert_eq!(manifest.sha256, MODEL_SHA256);
        assert_eq!(manifest.filename, "inference.onnx");
    }

    #[test]
    fn explicit_model_path_wins_and_is_fingerprinted_without_downloading() {
        let directory = tempfile::tempdir().unwrap();
        let model_path = directory.path().join("custom.onnx");
        std::fs::write(&model_path, b"custom model bytes").unwrap();
        let manifest = ModelDescriptor {
            filename: "model.onnx".to_owned(),
            url: "https://unused.invalid/model.onnx".to_owned(),
            sha256: format!("{:x}", Sha256::digest(b"custom model bytes")),
        };
        let selection = LayoutModelPathSelection {
            path: model_path.clone(),
            source: "flag",
        };

        let resolved = resolve_with_manifest(
            Some(&selection),
            &directory.path().join("cache"),
            Some("http://127.0.0.1:9"),
            &manifest,
        )
        .unwrap();

        assert_eq!(resolved.path, model_path);
        assert_eq!(resolved.source, format!("flag:{}", model_path.display()));
        assert_eq!(
            resolved.sha256,
            format!("{:x}", Sha256::digest(b"custom model bytes"))
        );
    }

    #[test]
    fn explicit_model_path_must_match_the_pinned_manifest_sha() {
        let directory = tempfile::tempdir().unwrap();
        let model_path = directory.path().join("wrong.onnx");
        std::fs::write(&model_path, b"wrong model bytes").unwrap();
        let selection = LayoutModelPathSelection {
            path: model_path,
            source: "flag",
        };
        let manifest = ModelDescriptor {
            filename: "model.onnx".to_owned(),
            url: "https://unused.invalid/model.onnx".to_owned(),
            sha256: format!("{:x}", Sha256::digest(b"expected model bytes")),
        };

        let error = resolve_with_manifest(
            Some(&selection),
            &directory.path().join("cache"),
            Some("http://127.0.0.1:9"),
            &manifest,
        )
        .unwrap_err();

        assert_eq!(
            error.reason(),
            ErrorReason::Asset(AssetReason::LayoutModelUnavailable)
        );
        assert_eq!(error.category().code(), 3);
    }

    #[test]
    fn loopback_download_is_sha_checked_then_reused_from_cache() {
        let bytes = b"deterministic fake ONNX bytes".to_vec();
        let manifest = ModelDescriptor {
            filename: "model.onnx".to_owned(),
            url: "https://unused.invalid/model.onnx".to_owned(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let cache = tempfile::tempdir().unwrap();

        let downloaded =
            resolve_with_manifest(None, cache.path(), Some(&base_url), &manifest).unwrap();
        server.join().unwrap();
        assert!(downloaded.source.starts_with("download:http://127.0.0.1:"));
        assert!(downloaded.path.is_file());

        let cached =
            resolve_with_manifest(None, cache.path(), Some("http://127.0.0.1:9"), &manifest)
                .unwrap();
        assert!(cached.source.starts_with("cache:"));
        assert_eq!(cached.sha256, manifest.sha256);
    }

    #[test]
    fn unreadable_explicit_model_is_an_asset_exit_three_error() {
        let directory = tempfile::tempdir().unwrap();
        let selection = LayoutModelPathSelection {
            path: directory.path().join("missing.onnx"),
            source: "flag",
        };
        let error = resolve_with_manifest(
            Some(&selection),
            directory.path(),
            None,
            &production_manifest(),
        )
        .unwrap_err();
        assert_eq!(
            error.reason(),
            ErrorReason::Asset(AssetReason::LayoutModelUnavailable)
        );
        assert_eq!(error.category().code(), 3);
    }
}
