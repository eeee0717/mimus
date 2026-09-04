use std::io::Read;
use std::path::Path;
use std::time::Duration;

use mimus_core::error::{AssetReason, MimusError, Result};
use mimus_core::{OutputFont, OutputFonts};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

use crate::config::FontPathSelection;

const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;
const NOTO_COMMIT: &str = "523d033d6cb47f4a80c58a35753646f5c3608a78";
const NOTO_SERIF_SC_VF_SHA256: &str =
    "69467baf421bdbb32b292d6c092ed033ca32e5f7a0d06194e69901287b50b2f3";
const STIX_FONTS_COMMIT: &str = "744a22a4dd626cd14d75728aef34fc8ad7c85db0";
const GOOGLE_FONTS_COMMIT: &str = "9017368e541f77a66e2302f474d2142d1bb77f5c";
const STIX_TWO_TEXT_VF_SHA256: &str =
    "7962b8b7811e6a896c9a91a0bccbb5241047770eb24d4997c5cb5fe21d5c0df2";
const STIX_TWO_MATH_SHA256: &str =
    "562551b15b836e6e01d1b7350909baf3c8c8d83260c1190fbf4544333e6936de";

#[derive(Debug, Clone)]
struct FontDescriptor {
    filename: &'static str,
    url: String,
    sha256: &'static str,
}

#[derive(Debug, Clone)]
struct FontManifest {
    regular: FontDescriptor,
    bold: FontDescriptor,
    latin_regular: FontDescriptor,
    latin_bold: FontDescriptor,
    latin_symbol: FontDescriptor,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FontSelections<'a> {
    pub regular: Option<&'a FontPathSelection>,
    pub bold: Option<&'a FontPathSelection>,
    pub latin_regular: Option<&'a FontPathSelection>,
    pub latin_bold: Option<&'a FontPathSelection>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FontCacheDirs<'a> {
    pub cjk: &'a Path,
    pub latin: &'a Path,
    pub latin_symbol: &'a Path,
}

pub(crate) fn resolve_fonts(
    selections: FontSelections<'_>,
    cache_dirs: FontCacheDirs<'_>,
    mirror: Option<&str>,
) -> Result<OutputFonts> {
    resolve_with_manifest(selections, cache_dirs, mirror, &production_manifest())
}

fn production_manifest() -> FontManifest {
    let url = format!(
        "https://raw.githubusercontent.com/notofonts/noto-cjk/{NOTO_COMMIT}/Serif/Variable/TTF/Subset/NotoSerifSC-VF.ttf"
    );
    let latin_url = format!(
        "https://raw.githubusercontent.com/stipub/stixfonts/{STIX_FONTS_COMMIT}/fonts/variable_ttf/STIXTwoText%5Bwght%5D.ttf"
    );
    FontManifest {
        regular: FontDescriptor {
            filename: "NotoSerifSC-VF.ttf",
            url: url.clone(),
            sha256: NOTO_SERIF_SC_VF_SHA256,
        },
        bold: FontDescriptor {
            filename: "NotoSerifSC-VF.ttf",
            url,
            sha256: NOTO_SERIF_SC_VF_SHA256,
        },
        latin_regular: FontDescriptor {
            filename: "STIXTwoText[wght].ttf",
            url: latin_url.clone(),
            sha256: STIX_TWO_TEXT_VF_SHA256,
        },
        latin_bold: FontDescriptor {
            filename: "STIXTwoText[wght].ttf",
            url: latin_url,
            sha256: STIX_TWO_TEXT_VF_SHA256,
        },
        latin_symbol: FontDescriptor {
            filename: "STIXTwoMath-Regular.ttf",
            url: format!(
                "https://raw.githubusercontent.com/google/fonts/{GOOGLE_FONTS_COMMIT}/ofl/stixtwomath/STIXTwoMath-Regular.ttf"
            ),
            sha256: STIX_TWO_MATH_SHA256,
        },
    }
}

fn resolve_with_manifest(
    selections: FontSelections<'_>,
    cache_dirs: FontCacheDirs<'_>,
    mirror: Option<&str>,
    manifest: &FontManifest,
) -> Result<OutputFonts> {
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| missing_fonts_error())?;
    let regular = resolve_one(
        selections.regular,
        cache_dirs.cjk,
        mirror,
        &manifest.regular,
        &client,
    )?;
    let bold = resolve_one(
        selections.bold,
        cache_dirs.cjk,
        mirror,
        &manifest.bold,
        &client,
    )?;
    let resolved_latin_regular = resolve_one(
        selections.latin_regular,
        cache_dirs.latin,
        mirror,
        &manifest.latin_regular,
        &client,
    )?;
    let resolved_latin_bold = resolve_one(
        selections.latin_bold,
        cache_dirs.latin,
        mirror,
        &manifest.latin_bold,
        &client,
    )?;
    let latin_symbol = if selections.latin_regular.is_some() || selections.latin_bold.is_some() {
        if selections.latin_regular.is_some() {
            resolved_latin_regular.clone()
        } else {
            resolved_latin_bold.clone()
        }
    } else {
        resolve_one(
            None,
            cache_dirs.latin_symbol,
            mirror,
            &manifest.latin_symbol,
            &client,
        )?
    };
    let fonts = OutputFonts {
        regular,
        bold,
        latin_regular: resolved_latin_regular,
        latin_bold: resolved_latin_bold,
        latin_symbol,
    };
    mimus_core::pass::validate_output_font_configuration(&fonts).map_err(|message| {
        MimusError::asset(AssetReason::OutputFontUnavailable, message).with_hint(
            "provide compatible CJK --font/--font-bold and Latin --font-latin/--font-latin-bold files",
        )
    })?;
    Ok(fonts)
}

fn resolve_one(
    explicit: Option<&FontPathSelection>,
    cache_dir: &Path,
    mirror: Option<&str>,
    descriptor: &FontDescriptor,
    client: &Client,
) -> Result<OutputFont> {
    if let Some(selection) = explicit {
        return load_custom_font(&selection.path, selection.source);
    }

    let cache_path = cache_dir.join(descriptor.filename);
    if let Ok(bytes) = std::fs::read(&cache_path)
        && sha256(&bytes) == descriptor.sha256
    {
        return parse_font(bytes, format!("cache:{}", cache_path.display()));
    }

    let url = asset_url(mirror, descriptor)?;
    let bytes = download(client, &url)?;
    let actual_sha256 = sha256(&bytes);
    if actual_sha256 != descriptor.sha256 {
        return Err(MimusError::asset(
            AssetReason::OutputFontUnavailable,
            format!(
                "downloaded output font failed SHA-256 validation: expected {}, got {actual_sha256}",
                descriptor.sha256
            ),
        )
        .with_hint(
            "check the configured asset mirror or provide --font, --font-bold, --font-latin, and --font-latin-bold",
        ));
    }
    let font = parse_font(bytes.clone(), format!("download:{url}"))?;
    publish_cache(&cache_path, &bytes)?;
    Ok(font)
}

fn load_custom_font(path: &Path, source: &'static str) -> Result<OutputFont> {
    let bytes = std::fs::read(path).map_err(|_| {
        MimusError::asset(
            AssetReason::OutputFontUnavailable,
            format!("could not read output font {}", path.display()),
        )
        .with_hint("provide readable CJK and Latin Regular and Bold font files")
    })?;
    parse_font(bytes, format!("{source}:{}", path.display()))
}

fn parse_font(bytes: Vec<u8>, source: String) -> Result<OutputFont> {
    let face = ttf_parser::Face::parse(&bytes, 0).map_err(|_| {
        MimusError::asset(
            AssetReason::OutputFontUnavailable,
            "output font is not a supported TTF or OpenType font",
        )
        .with_hint("provide TTF or OpenType CJK and Latin Regular and Bold font files")
    })?;
    let postscript_name = face
        .names()
        .into_iter()
        .find(|name| name.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
        .and_then(|name| name.to_string())
        .map(|name| sanitize_pdf_name(&name))
        .unwrap_or_else(|| "MimusOutput".to_owned());
    let sha256 = sha256(&bytes);
    Ok(OutputFont {
        bytes,
        postscript_name,
        source,
        sha256,
    })
}

fn asset_url(mirror: Option<&str>, descriptor: &FontDescriptor) -> Result<String> {
    let value = mirror.map_or_else(
        || descriptor.url.clone(),
        |base| format!("{}/{}", base.trim_end_matches('/'), descriptor.filename),
    );
    let url = reqwest::Url::parse(&value).map_err(|_| missing_fonts_error())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(MimusError::asset(
            AssetReason::OutputFontUnavailable,
            "asset mirror must be an http(s) base URL without credentials, query, or fragment",
        ));
    }
    Ok(value)
}

fn download(client: &Client, url: &str) -> Result<Vec<u8>> {
    let response = client.get(url).send().map_err(|_| missing_fonts_error())?;
    if !response.status().is_success() {
        return Err(missing_fonts_error());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_FONT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| missing_fonts_error())?;
    if bytes.len() as u64 > MAX_FONT_BYTES {
        return Err(MimusError::asset(
            AssetReason::OutputFontUnavailable,
            "downloaded output font exceeds the 64 MiB limit",
        ));
    }
    Ok(bytes)
}

fn publish_cache(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().ok_or_else(missing_fonts_error)?;
    std::fs::create_dir_all(directory).map_err(|_| missing_fonts_error())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(directory).map_err(|_| missing_fonts_error())?;
    std::io::Write::write_all(&mut temporary, bytes).map_err(|_| missing_fonts_error())?;
    temporary.persist(path).map_err(|_| missing_fonts_error())?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sanitize_pdf_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
        .collect::<String>();
    if sanitized.is_empty() {
        "MimusOutput".to_owned()
    } else {
        sanitized
    }
}

fn missing_fonts_error() -> MimusError {
    MimusError::asset(
        AssetReason::OutputFontUnavailable,
        "CJK and Latin Regular and Bold output fonts could not be resolved",
    )
    .with_hint(
        "provide --font, --font-bold, --font-latin, and --font-latin-bold or configure MIMUS_ASSET_MIRROR",
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;

    use super::*;

    fn test_font(weight: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/assets/fonts")
            .join(format!("MimusTestGB2312-{weight}.ttf"))
    }

    fn test_latin_font() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/assets/fonts")
            .join("MimusTestLatin.ttf")
    }

    #[test]
    fn loopback_download_is_sha_checked_published_and_then_read_from_cache() {
        let regular = std::fs::read(test_font("Regular")).unwrap();
        let bold = std::fs::read(test_font("Bold")).unwrap();
        let latin_regular = std::fs::read(test_latin_font()).unwrap();
        let latin_bold = latin_regular.clone();
        let latin_symbol = latin_regular.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for bytes in [regular, bold, latin_regular, latin_bold, latin_symbol] {
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
            }
        });
        let manifest = FontManifest {
            regular: FontDescriptor {
                filename: "regular.ttf",
                url: "https://unused.invalid/regular.ttf".to_owned(),
                sha256: "510d0470ca8b77f035fe8e7143526207088c1bdad017451cf253020f72397d63",
            },
            bold: FontDescriptor {
                filename: "bold.ttf",
                url: "https://unused.invalid/bold.ttf".to_owned(),
                sha256: "1a917349eb06866f5701532f0cea586d184edadbd1cfdd3f034f3a18f2ff5316",
            },
            latin_regular: FontDescriptor {
                filename: "latin-regular.ttf",
                url: "https://unused.invalid/latin-regular.ttf".to_owned(),
                sha256: "621539180203f4667d247c49c8bf4102112b28e1627190ca625ebd1e61848a5f",
            },
            latin_bold: FontDescriptor {
                filename: "latin-bold.ttf",
                url: "https://unused.invalid/latin-bold.ttf".to_owned(),
                sha256: "621539180203f4667d247c49c8bf4102112b28e1627190ca625ebd1e61848a5f",
            },
            latin_symbol: FontDescriptor {
                filename: "latin-symbol.ttf",
                url: "https://unused.invalid/latin-symbol.ttf".to_owned(),
                sha256: "621539180203f4667d247c49c8bf4102112b28e1627190ca625ebd1e61848a5f",
            },
        };
        let cache = tempfile::tempdir().unwrap();
        let cjk_cache = cache.path().join("cjk");
        let latin_cache = cache.path().join("latin");
        let symbol_cache = cache.path().join("symbol");
        let selections = FontSelections {
            regular: None,
            bold: None,
            latin_regular: None,
            latin_bold: None,
        };
        let cache_dirs = FontCacheDirs {
            cjk: &cjk_cache,
            latin: &latin_cache,
            latin_symbol: &symbol_cache,
        };

        let downloaded =
            resolve_with_manifest(selections, cache_dirs, Some(&base_url), &manifest).unwrap();
        server.join().unwrap();
        assert!(
            downloaded
                .regular
                .source
                .starts_with("download:http://127.0.0.1:")
        );
        assert!(cjk_cache.join("regular.ttf").is_file());
        assert!(cjk_cache.join("bold.ttf").is_file());
        assert!(latin_cache.join("latin-regular.ttf").is_file());
        assert!(latin_cache.join("latin-bold.ttf").is_file());
        assert!(symbol_cache.join("latin-symbol.ttf").is_file());

        let cached = resolve_with_manifest(
            selections,
            cache_dirs,
            Some("http://127.0.0.1:9"),
            &manifest,
        )
        .unwrap();
        assert!(cached.regular.source.starts_with("cache:"));
        assert!(cached.bold.source.starts_with("cache:"));
        assert!(cached.latin_regular.source.starts_with("cache:"));
        assert!(cached.latin_bold.source.starts_with("cache:"));
        assert!(cached.latin_symbol.source.starts_with("cache:"));
    }

    #[test]
    fn pdf_font_names_are_restricted_to_safe_name_characters() {
        assert_eq!(sanitize_pdf_name("Noto Sans SC/Bold"), "NotoSansSCBold");
        assert_eq!(sanitize_pdf_name("[]"), "MimusOutput");
    }

    #[test]
    fn production_manifest_pins_noto_serif_sc_for_both_primary_slots() {
        let manifest = production_manifest();
        for descriptor in [&manifest.regular, &manifest.bold] {
            assert_eq!(descriptor.filename, "NotoSerifSC-VF.ttf");
            assert_eq!(descriptor.sha256, NOTO_SERIF_SC_VF_SHA256);
            assert_eq!(
                descriptor.url,
                format!(
                    "https://raw.githubusercontent.com/notofonts/noto-cjk/{NOTO_COMMIT}/Serif/Variable/TTF/Subset/NotoSerifSC-VF.ttf"
                )
            );
        }
    }

    #[test]
    fn production_manifest_pins_stix_text_and_math_without_dejavu() {
        let manifest = production_manifest();
        for descriptor in [&manifest.latin_regular, &manifest.latin_bold] {
            assert_eq!(descriptor.filename, "STIXTwoText[wght].ttf");
            assert_eq!(descriptor.sha256, STIX_TWO_TEXT_VF_SHA256);
            assert_eq!(
                descriptor.url,
                format!(
                    "https://raw.githubusercontent.com/stipub/stixfonts/{STIX_FONTS_COMMIT}/fonts/variable_ttf/STIXTwoText%5Bwght%5D.ttf"
                )
            );
        }
        assert_eq!(manifest.latin_symbol.filename, "STIXTwoMath-Regular.ttf");
        assert_eq!(manifest.latin_symbol.sha256, STIX_TWO_MATH_SHA256);
        assert_eq!(
            manifest.latin_symbol.url,
            format!(
                "https://raw.githubusercontent.com/google/fonts/{GOOGLE_FONTS_COMMIT}/ofl/stixtwomath/STIXTwoMath-Regular.ttf"
            )
        );
    }

    #[test]
    fn test_font_covers_gb2312_level_one_scale_and_common_punctuation() {
        let bytes = std::fs::read(test_font("Regular")).unwrap();
        let face = ttf_parser::Face::parse(&bytes, 0).unwrap();
        let han_glyphs = ('\u{4e00}'..='\u{9fff}')
            .filter_map(|value| face.glyph_index(value))
            .collect::<std::collections::BTreeSet<_>>();
        let han_coverage = han_glyphs.len();
        assert!(han_coverage >= 3_755, "Han coverage was {han_coverage}");
        for value in "，。！？：“”‘’（）《》【】；、—…·".chars() {
            assert!(face.glyph_index(value).is_some(), "missing {value}");
        }
    }

    #[test]
    fn test_latin_font_covers_routing_and_symbol_oracles() {
        let bytes = std::fs::read(test_latin_font()).unwrap();
        let face = ttf_parser::Face::parse(&bytes, 0).unwrap();
        for value in "AZaz09.,Łϵ∗“".chars() {
            assert!(face.glyph_index(value).is_some(), "missing {value}");
        }
    }
}
