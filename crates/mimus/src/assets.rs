use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mimus_core::error::{AssetReason, MimusError, Result};
use mimus_core::event::{AssetManifestEntry, Event, EventKind, EventSink};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

const PROGRESS_INTERVAL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssetId {
    NotoSerifSc,
    StixTwoText,
    StixTwoMath,
    PpDocLayoutV3,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AssetDescriptor {
    pub id: AssetId,
    pub name: &'static str,
    pub kind: &'static str,
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub cache_path: &'static str,
    pub max_bytes: u64,
    pub timeout: Duration,
    pub reason: AssetReason,
    pub hint: &'static str,
}

pub(crate) const MANIFEST: [AssetDescriptor; 4] = [
    AssetDescriptor {
        id: AssetId::NotoSerifSc,
        name: "noto-serif-sc",
        kind: "font",
        version: "2.001",
        url: "https://raw.githubusercontent.com/notofonts/noto-cjk/523d033d6cb47f4a80c58a35753646f5c3608a78/Serif/Variable/TTF/Subset/NotoSerifSC-VF.ttf",
        sha256: "69467baf421bdbb32b292d6c092ed033ca32e5f7a0d06194e69901287b50b2f3",
        cache_path: "fonts/noto-serif-sc-2.001/NotoSerifSC-VF.ttf",
        max_bytes: 64 * 1024 * 1024,
        timeout: Duration::from_secs(60),
        reason: AssetReason::OutputFontUnavailable,
        hint: "provide --font and --font-bold or check the configured asset mirror",
    },
    AssetDescriptor {
        id: AssetId::StixTwoText,
        name: "stix-two-text",
        kind: "font",
        version: "2.13b171",
        url: "https://raw.githubusercontent.com/stipub/stixfonts/744a22a4dd626cd14d75728aef34fc8ad7c85db0/fonts/variable_ttf/STIXTwoText%5Bwght%5D.ttf",
        sha256: "7962b8b7811e6a896c9a91a0bccbb5241047770eb24d4997c5cb5fe21d5c0df2",
        cache_path: "fonts/stix-two-text-2.13b171/STIXTwoText[wght].ttf",
        max_bytes: 64 * 1024 * 1024,
        timeout: Duration::from_secs(60),
        reason: AssetReason::OutputFontUnavailable,
        hint: "provide --font-latin and --font-latin-bold or check the configured asset mirror",
    },
    AssetDescriptor {
        id: AssetId::StixTwoMath,
        name: "stix-two-math",
        kind: "font",
        version: "2.12b168a",
        url: "https://raw.githubusercontent.com/google/fonts/9017368e541f77a66e2302f474d2142d1bb77f5c/ofl/stixtwomath/STIXTwoMath-Regular.ttf",
        sha256: "562551b15b836e6e01d1b7350909baf3c8c8d83260c1190fbf4544333e6936de",
        cache_path: "fonts/stix-two-math-2.12b168a/STIXTwoMath-Regular.ttf",
        max_bytes: 64 * 1024 * 1024,
        timeout: Duration::from_secs(60),
        reason: AssetReason::OutputFontUnavailable,
        hint: "provide --font-latin and --font-latin-bold or check the configured asset mirror",
    },
    AssetDescriptor {
        id: AssetId::PpDocLayoutV3,
        name: "pp-doclayoutv3",
        kind: "model",
        version: "46bbdf188bb0a772c08aed74882ce7e51a8f1ea6",
        url: "https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx/resolve/46bbdf188bb0a772c08aed74882ce7e51a8f1ea6/inference.onnx",
        sha256: "45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba",
        cache_path: "models/pp-doclayoutv3-46bbdf188bb0a772c08aed74882ce7e51a8f1ea6/inference.onnx",
        max_bytes: 160 * 1024 * 1024,
        timeout: Duration::from_secs(180),
        reason: AssetReason::LayoutModelUnavailable,
        hint: "provide --layout-model or check the configured asset mirror",
    },
];

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAsset {
    pub bytes: Vec<u8>,
    pub path: PathBuf,
    pub source: String,
    pub sha256: String,
}

pub(crate) fn descriptor(id: AssetId) -> &'static AssetDescriptor {
    MANIFEST
        .iter()
        .find(|descriptor| descriptor.id == id)
        .expect("every asset id is present in the manifest")
}

pub(crate) fn manifest_entries() -> Vec<AssetManifestEntry> {
    MANIFEST
        .iter()
        .map(|descriptor| AssetManifestEntry {
            name: descriptor.name.to_owned(),
            kind: descriptor.kind.to_owned(),
            version: descriptor.version.to_owned(),
            url: descriptor.url.to_owned(),
            sha256: descriptor.sha256.to_owned(),
            cache_path: descriptor.cache_path.to_owned(),
        })
        .collect()
}

pub(crate) fn resolve_managed_asset(
    descriptor: &AssetDescriptor,
    cache_root: &Path,
    mirror: Option<&str>,
    events: &dyn EventSink,
    validate: impl Fn(&[u8]) -> Result<()>,
) -> Result<ResolvedAsset> {
    let cache_path = cache_root.join(descriptor.cache_path);
    if let Ok(bytes) = std::fs::read(&cache_path) {
        if sha256(&bytes) == descriptor.sha256 {
            if let Err(error) = validate(&bytes) {
                std::fs::remove_file(&cache_path).map_err(|remove_error| {
                    asset_error(
                        descriptor,
                        format!(
                            "cached asset is incompatible and could not be removed: {remove_error}"
                        ),
                    )
                })?;
                return Err(error);
            }
            return Ok(ResolvedAsset {
                bytes,
                path: cache_path.clone(),
                source: format!("cache:{}", cache_path.display()),
                sha256: descriptor.sha256.to_owned(),
            });
        }
        std::fs::remove_file(&cache_path).map_err(|error| {
            asset_error(
                descriptor,
                format!("could not remove invalid cached asset: {error}"),
            )
        })?;
    }

    let url = asset_url(mirror, descriptor)?;
    let directory = cache_path
        .parent()
        .ok_or_else(|| asset_error(descriptor, "asset cache path has no parent directory"))?;
    std::fs::create_dir_all(directory).map_err(|error| {
        asset_error(descriptor, format!("could not create asset cache: {error}"))
    })?;
    let client = Client::builder()
        .timeout(descriptor.timeout)
        .build()
        .map_err(|error| {
            asset_error(descriptor, format!("could not create HTTP client: {error}"))
        })?;

    events.emit(Event::new(EventKind::AssetDownloadStarted {
        name: descriptor.name.to_owned(),
        version: descriptor.version.to_owned(),
        bytes: 0,
        sha256: descriptor.sha256.to_owned(),
    }))?;
    let mut response = client
        .get(&url)
        .send()
        .map_err(|error| asset_error(descriptor, format!("asset download failed: {error}")))?;
    if !response.status().is_success() {
        return Err(asset_error(
            descriptor,
            format!("asset download returned HTTP {}", response.status()),
        ));
    }
    let expected_bytes = response.content_length();
    if expected_bytes.is_some_and(|bytes| bytes == 0 || bytes > descriptor.max_bytes) {
        return Err(asset_error(
            descriptor,
            format!(
                "asset content length exceeds the {} byte limit",
                descriptor.max_bytes
            ),
        ));
    }

    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(|error| {
        asset_error(
            descriptor,
            format!("could not create asset temporary file: {error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut downloaded_bytes = 0_u64;
    let mut next_progress = PROGRESS_INTERVAL_BYTES;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = response.read(&mut buffer).map_err(|error| {
            asset_error(
                descriptor,
                format!("asset download was interrupted: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        downloaded_bytes = downloaded_bytes.saturating_add(read as u64);
        if downloaded_bytes > descriptor.max_bytes {
            return Err(asset_error(
                descriptor,
                format!(
                    "downloaded asset exceeds the {} byte limit",
                    descriptor.max_bytes
                ),
            ));
        }
        temporary.write_all(&buffer[..read]).map_err(|error| {
            asset_error(
                descriptor,
                format!("could not write asset temporary file: {error}"),
            )
        })?;
        hasher.update(&buffer[..read]);
        if downloaded_bytes >= next_progress {
            events.emit(Event::new(EventKind::AssetDownloadProgress {
                name: descriptor.name.to_owned(),
                bytes: downloaded_bytes,
                total_bytes: expected_bytes,
                sha256: descriptor.sha256.to_owned(),
            }))?;
            next_progress = downloaded_bytes.saturating_add(PROGRESS_INTERVAL_BYTES);
        }
    }
    if downloaded_bytes == 0 || expected_bytes.is_some_and(|expected| expected != downloaded_bytes)
    {
        return Err(asset_error(
            descriptor,
            "asset download ended before the declared content length",
        ));
    }
    events.emit(Event::new(EventKind::AssetDownloadProgress {
        name: descriptor.name.to_owned(),
        bytes: downloaded_bytes,
        total_bytes: expected_bytes,
        sha256: descriptor.sha256.to_owned(),
    }))?;
    temporary.as_file_mut().sync_all().map_err(|error| {
        asset_error(
            descriptor,
            format!("could not sync asset temporary file: {error}"),
        )
    })?;
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if actual_sha256 != descriptor.sha256 {
        return Err(asset_error(
            descriptor,
            format!(
                "downloaded asset failed SHA-256 validation: expected {}, got {actual_sha256}",
                descriptor.sha256
            ),
        ));
    }
    let bytes = std::fs::read(temporary.path()).map_err(|error| {
        asset_error(
            descriptor,
            format!("could not reread downloaded asset: {error}"),
        )
    })?;
    validate(&bytes)?;
    temporary.persist(&cache_path).map_err(|error| {
        asset_error(
            descriptor,
            format!("could not publish cached asset: {error}"),
        )
    })?;
    events.emit(Event::new(EventKind::AssetDownloadFinished {
        name: descriptor.name.to_owned(),
        bytes: downloaded_bytes,
        sha256: actual_sha256.clone(),
        cache_path: descriptor.cache_path.to_owned(),
    }))?;

    Ok(ResolvedAsset {
        bytes,
        path: cache_path,
        source: format!("download:{url}"),
        sha256: actual_sha256,
    })
}

fn asset_url(mirror: Option<&str>, descriptor: &AssetDescriptor) -> Result<String> {
    let filename = Path::new(descriptor.cache_path)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| asset_error(descriptor, "asset cache filename is invalid"))?;
    let value = mirror.map_or_else(
        || descriptor.url.to_owned(),
        |base| format!("{}/{filename}", base.trim_end_matches('/')),
    );
    let parsed =
        reqwest::Url::parse(&value).map_err(|_| asset_error(descriptor, "asset URL is invalid"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(asset_error(
            descriptor,
            "asset mirror must be an http(s) base URL without credentials, query, or fragment",
        ));
    }
    Ok(value)
}

fn asset_error(descriptor: &AssetDescriptor, message: impl Into<String>) -> MimusError {
    MimusError::asset(descriptor.reason, message).with_hint(descriptor.hint)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use mimus_core::event::{EventKind, RecordingEventSink};

    use super::*;

    fn test_descriptor(bytes: &[u8]) -> AssetDescriptor {
        AssetDescriptor {
            id: AssetId::PpDocLayoutV3,
            name: "test-asset",
            kind: "model",
            version: "1",
            url: "https://unused.invalid/asset.bin",
            sha256: Box::leak(sha256(bytes).into_boxed_str()),
            cache_path: "models/test/asset.bin",
            max_bytes: 1024,
            timeout: Duration::from_secs(2),
            reason: AssetReason::LayoutModelUnavailable,
            hint: "test hint",
        }
    }

    fn serve_once(body: Vec<u8>, declared_length: usize) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /asset.bin "));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        (base_url, server)
    }

    #[test]
    fn manifest_has_the_four_pinned_backward_compatible_assets() {
        let entries = manifest_entries();
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.cache_path.as_str())
                .collect::<Vec<_>>(),
            [
                "fonts/noto-serif-sc-2.001/NotoSerifSC-VF.ttf",
                "fonts/stix-two-text-2.13b171/STIXTwoText[wght].ttf",
                "fonts/stix-two-math-2.12b168a/STIXTwoMath-Regular.ttf",
                "models/pp-doclayoutv3-46bbdf188bb0a772c08aed74882ce7e51a8f1ea6/inference.onnx",
            ]
        );
        assert!(entries.iter().all(|entry| !entry.url.contains("DejaVu")));
    }

    #[test]
    fn mirror_download_emits_progress_publishes_atomically_and_reuses_cache_offline() {
        let bytes = b"deterministic asset bytes".to_vec();
        let descriptor = test_descriptor(&bytes);
        let (mirror, server) = serve_once(bytes.clone(), bytes.len());
        let cache = tempfile::tempdir().unwrap();
        let events = RecordingEventSink::default();

        let downloaded = resolve_managed_asset(
            &descriptor,
            cache.path(),
            Some(&mirror),
            &events,
            |_| Ok(()),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(downloaded.bytes, bytes);
        assert!(downloaded.source.starts_with("download:http://127.0.0.1:"));
        assert_eq!(std::fs::read(&downloaded.path).unwrap(), downloaded.bytes);
        let kinds = events
            .events()
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        assert!(matches!(
            kinds.first(),
            Some(EventKind::AssetDownloadStarted { .. })
        ));
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, EventKind::AssetDownloadProgress { .. }))
        );
        assert!(matches!(
            kinds.last(),
            Some(EventKind::AssetDownloadFinished { .. })
        ));

        let offline_events = RecordingEventSink::default();
        let cached = resolve_managed_asset(
            &descriptor,
            cache.path(),
            Some("http://127.0.0.1:9"),
            &offline_events,
            |_| Ok(()),
        )
        .unwrap();
        assert!(cached.source.starts_with("cache:"));
        assert!(offline_events.events().is_empty());
    }

    #[test]
    fn hash_mismatch_and_interrupted_download_leave_no_cache_or_temporary_file() {
        for (body, declared_length) in [
            (b"wrong bytes".to_vec(), b"wrong bytes".len()),
            (b"partial".to_vec(), b"partial payload".len()),
        ] {
            let descriptor = test_descriptor(b"expected bytes");
            let (mirror, server) = serve_once(body, declared_length);
            let cache = tempfile::tempdir().unwrap();
            let error = resolve_managed_asset(
                &descriptor,
                cache.path(),
                Some(&mirror),
                &RecordingEventSink::default(),
                |_| Ok(()),
            )
            .unwrap_err();
            server.join().unwrap();
            assert_eq!(error.category().code(), 3);
            let target = cache.path().join(descriptor.cache_path);
            assert!(!target.exists());
            let directory = target.parent().unwrap();
            assert!(!directory.exists() || std::fs::read_dir(directory).unwrap().next().is_none());
        }
    }

    #[test]
    fn compatibility_failure_is_exit_three_and_is_not_published() {
        let bytes = b"incompatible bytes".to_vec();
        let descriptor = test_descriptor(&bytes);
        let (mirror, server) = serve_once(bytes.clone(), bytes.len());
        let cache = tempfile::tempdir().unwrap();
        let error = resolve_managed_asset(
            &descriptor,
            cache.path(),
            Some(&mirror),
            &RecordingEventSink::default(),
            |_| Err(asset_error(&descriptor, "incompatible asset")),
        )
        .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.category().code(), 3);
        assert!(!cache.path().join(descriptor.cache_path).exists());
    }

    #[test]
    fn incompatible_cached_asset_is_removed() {
        let bytes = b"incompatible cached bytes".to_vec();
        let descriptor = test_descriptor(&bytes);
        let cache = tempfile::tempdir().unwrap();
        let target = cache.path().join(descriptor.cache_path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, &bytes).unwrap();

        let error = resolve_managed_asset(
            &descriptor,
            cache.path(),
            Some("http://127.0.0.1:9"),
            &RecordingEventSink::default(),
            |_| Err(asset_error(&descriptor, "incompatible asset")),
        )
        .unwrap_err();

        assert_eq!(error.category().code(), 3);
        assert!(!target.exists());
    }

    #[test]
    fn atomic_publish_replaces_a_target_created_during_validation() {
        let bytes = b"replacement bytes".to_vec();
        let descriptor = test_descriptor(&bytes);
        let (mirror, server) = serve_once(bytes.clone(), bytes.len());
        let cache = tempfile::tempdir().unwrap();
        let target = cache.path().join(descriptor.cache_path);

        let resolved = resolve_managed_asset(
            &descriptor,
            cache.path(),
            Some(&mirror),
            &RecordingEventSink::default(),
            |_| {
                std::fs::write(&target, b"concurrent stale bytes").unwrap();
                Ok(())
            },
        )
        .unwrap();
        server.join().unwrap();

        assert_eq!(resolved.bytes, bytes);
        assert_eq!(std::fs::read(&target).unwrap(), resolved.bytes);
    }
}
