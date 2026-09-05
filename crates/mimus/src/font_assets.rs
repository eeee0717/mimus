use std::path::Path;

use mimus_core::error::{AssetReason, MimusError, Result};
use mimus_core::event::EventSink;
use mimus_core::{OutputFont, OutputFonts};
use sha2::{Digest, Sha256};

use crate::assets::{self, AssetId};
use crate::config::FontPathSelection;

#[derive(Debug, Clone, Copy)]
pub(crate) struct FontSelections<'a> {
    pub regular: Option<&'a FontPathSelection>,
    pub bold: Option<&'a FontPathSelection>,
    pub latin_regular: Option<&'a FontPathSelection>,
    pub latin_bold: Option<&'a FontPathSelection>,
}

pub(crate) fn resolve_fonts(
    selections: FontSelections<'_>,
    cache_root: &Path,
    mirror: Option<&str>,
    events: &dyn EventSink,
) -> Result<OutputFonts> {
    let managed_cjk = if selections.regular.is_none() || selections.bold.is_none() {
        Some(resolve_default_font(
            AssetId::NotoSerifSc,
            cache_root,
            mirror,
            events,
        )?)
    } else {
        None
    };
    let managed_latin = if selections.latin_regular.is_none() || selections.latin_bold.is_none() {
        Some(resolve_default_font(
            AssetId::StixTwoText,
            cache_root,
            mirror,
            events,
        )?)
    } else {
        None
    };

    let regular = resolve_slot(selections.regular, managed_cjk.as_ref())?;
    let bold = resolve_slot(selections.bold, managed_cjk.as_ref())?;
    let latin_regular = resolve_slot(selections.latin_regular, managed_latin.as_ref())?;
    let latin_bold = resolve_slot(selections.latin_bold, managed_latin.as_ref())?;
    let latin_symbol = if selections.latin_regular.is_some() || selections.latin_bold.is_some() {
        if selections.latin_regular.is_some() {
            latin_regular.clone()
        } else {
            latin_bold.clone()
        }
    } else {
        resolve_default_font(AssetId::StixTwoMath, cache_root, mirror, events)?
    };
    let fonts = OutputFonts {
        regular,
        bold,
        latin_regular,
        latin_bold,
        latin_symbol,
    };
    mimus_core::pass::validate_output_font_configuration(&fonts).map_err(|message| {
        MimusError::asset(AssetReason::OutputFontUnavailable, message).with_hint(
            "provide compatible CJK --font/--font-bold and Latin --font-latin/--font-latin-bold files",
        )
    })?;
    Ok(fonts)
}

fn resolve_default_font(
    id: AssetId,
    cache_root: &Path,
    mirror: Option<&str>,
    events: &dyn EventSink,
) -> Result<OutputFont> {
    let descriptor = assets::descriptor(id);
    let resolved =
        assets::resolve_managed_asset(descriptor, cache_root, mirror, events, |bytes| {
            validate_font_bytes(bytes)
        })?;
    parse_font(resolved.bytes, resolved.source)
}

fn resolve_slot(
    selection: Option<&FontPathSelection>,
    managed: Option<&OutputFont>,
) -> Result<OutputFont> {
    match selection {
        Some(selection) => load_custom_font(&selection.path, selection.source),
        None => Ok(managed
            .expect("a missing explicit slot always resolves the managed family")
            .clone()),
    }
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

fn validate_font_bytes(bytes: &[u8]) -> Result<()> {
    ttf_parser::Face::parse(bytes, 0).map_err(|_| {
        MimusError::asset(
            AssetReason::OutputFontUnavailable,
            "output font is not a supported TTF or OpenType font",
        )
        .with_hint("provide TTF or OpenType CJK and Latin Regular and Bold font files")
    })?;
    Ok(())
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
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    Ok(OutputFont {
        bytes,
        postscript_name,
        source,
        sha256,
    })
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use mimus_core::event::NoopEventSink;

    use super::*;

    fn test_font(weight: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/assets/fonts")
            .join(format!("MimusTestGB2312-{weight}.ttf"))
    }

    fn test_latin_font() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets/fonts/MimusTestLatin.ttf")
    }

    fn explicit_fonts(cjk_regular: &Path, cjk_bold: &Path) -> OutputFonts {
        let cjk_regular = FontPathSelection {
            path: cjk_regular.to_owned(),
            source: "test",
        };
        let cjk_bold = FontPathSelection {
            path: cjk_bold.to_owned(),
            source: "test",
        };
        let latin = FontPathSelection {
            path: test_latin_font(),
            source: "test",
        };
        resolve_fonts(
            FontSelections {
                regular: Some(&cjk_regular),
                bold: Some(&cjk_bold),
                latin_regular: Some(&latin),
                latin_bold: Some(&latin),
            },
            Path::new("unused"),
            Some("http://127.0.0.1:9"),
            &NoopEventSink,
        )
        .unwrap()
    }

    #[test]
    fn explicit_static_fonts_bypass_network_and_pass_compatibility_checks() {
        let fonts = explicit_fonts(&test_font("Regular"), &test_font("Bold"));
        assert!(fonts.regular.source.starts_with("test:"));
        assert!(fonts.latin_regular.source.starts_with("test:"));
    }

    #[test]
    fn explicit_noto_sans_variable_font_instantiates_regular_and_bold() {
        let Some(path) = std::env::var_os("MIMUS_PINNED_SANS_FONT").map(PathBuf::from) else {
            return;
        };
        let fonts = explicit_fonts(&path, &path);
        assert_eq!(fonts.regular.sha256, fonts.bold.sha256);
        assert_ne!(fonts.regular.bytes.len(), 0);
    }

    #[test]
    fn pdf_font_names_are_restricted_to_safe_name_characters() {
        assert_eq!(sanitize_pdf_name("Noto Sans SC/Bold"), "NotoSansSCBold");
        assert_eq!(sanitize_pdf_name("[]"), "MimusOutput");
    }

    #[test]
    fn test_font_covers_gb2312_level_one_scale_and_common_punctuation() {
        let bytes = std::fs::read(test_font("Regular")).unwrap();
        let face = ttf_parser::Face::parse(&bytes, 0).unwrap();
        let han_coverage = ('\u{4e00}'..='\u{9fff}')
            .filter_map(|value| face.glyph_index(value))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert!(han_coverage >= 3_755, "Han coverage was {han_coverage}");
        for value in "，。！？：“”‘’（）《》【】；、—…·".chars() {
            assert!(face.glyph_index(value).is_some(), "missing {value}");
        }
    }
}
