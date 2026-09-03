use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ValueEnum;
use mimus_core::error::{MimusError, Result, UsageReason};
use mimus_core::translate::{Glossary, NoneTranslator, OpenAiTranslator, Translator};
use secrecy::SecretString;
use serde::Deserialize;

const DEFAULT_BASE_URL: &str = "https://api.openai.com";
const DEFAULT_MODEL: &str = "gpt-4.1-mini";
const DEFAULT_TARGET_LANGUAGE: &str = "zh-CN";
const DEFAULT_REQUEST_TIMEOUT_SECS: i64 = 120;
const MAX_REQUEST_TIMEOUT_SECS: i64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Backend {
    Openai,
    None,
}

impl Backend {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::None => "none",
        }
    }
}

pub(crate) struct ConfigOverrides {
    pub backend: Option<Backend>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub target_language: Option<String>,
    pub font_regular: Option<PathBuf>,
    pub font_bold: Option<PathBuf>,
    pub font_fallback_regular: Option<PathBuf>,
    pub font_fallback_bold: Option<PathBuf>,
    pub layout_model: Option<PathBuf>,
    pub asset_mirror: Option<String>,
    pub glossary: Option<PathBuf>,
    pub dump_glossary: Option<PathBuf>,
    pub no_auto_terms: bool,
    pub cache: Option<PathBuf>,
    pub no_cache: bool,
    pub concurrency: Option<usize>,
    pub request_timeout_secs: Option<i64>,
    pub strict: bool,
    pub translate_table: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FontPathSelection {
    pub path: PathBuf,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct LayoutModelPathSelection {
    pub path: PathBuf,
    pub source: &'static str,
}

pub(crate) struct ResolvedConfig {
    pub backend: Backend,
    pub base_url: String,
    pub model: String,
    pub target_language: String,
    pub font_regular: Option<FontPathSelection>,
    pub font_bold: Option<FontPathSelection>,
    pub font_fallback_regular: Option<FontPathSelection>,
    pub font_fallback_bold: Option<FontPathSelection>,
    pub layout_model: Option<LayoutModelPathSelection>,
    pub asset_mirror: Option<String>,
    pub font_cache_dir: PathBuf,
    pub layout_model_cache_dir: PathBuf,
    pub user_glossary: Glossary,
    pub dump_glossary: Option<PathBuf>,
    pub auto_terms: bool,
    pub cache_path: Option<PathBuf>,
    pub max_concurrency: usize,
    pub request_timeout_secs: u64,
    pub strict: bool,
    pub translate_table: bool,
    api_key: Option<SecretString>,
}

pub(crate) struct ResolvedLayoutConfig {
    pub layout_model: Option<LayoutModelPathSelection>,
    pub asset_mirror: Option<String>,
    pub layout_model_cache_dir: PathBuf,
}

impl ResolvedConfig {
    pub(crate) fn load(overrides: ConfigOverrides) -> Result<Self> {
        load_dotenv_local()?;
        let file = read_file_config()?;
        let environment = EnvironmentConfig::read();

        let backend = match overrides.backend {
            Some(backend) => backend,
            None => environment
                .backend
                .transpose()?
                .or(file.backend)
                .unwrap_or(Backend::Openai),
        };
        let base_url = choose(
            overrides.base_url,
            environment.base_url,
            file.base_url,
            DEFAULT_BASE_URL,
            "base URL",
        )?;
        mimus_core::translate::validate_openai_base_url(&base_url)?;
        let model = choose(
            overrides.model,
            environment.model,
            file.model,
            DEFAULT_MODEL,
            "model",
        )?;
        let target_language = choose(
            overrides.target_language,
            environment.target_language,
            file.target_language,
            DEFAULT_TARGET_LANGUAGE,
            "target language",
        )?;
        let max_concurrency = match overrides.concurrency {
            Some(value) => value,
            None => environment
                .concurrency
                .transpose()?
                .or(file.concurrency)
                .unwrap_or(4),
        };
        if max_concurrency == 0 {
            return Err(MimusError::usage(
                UsageReason::InvalidArguments,
                "translation concurrency must be at least 1",
            ));
        }
        let request_timeout_secs =
            validate_request_timeout_secs(match overrides.request_timeout_secs {
                Some(value) => value,
                None => environment
                    .request_timeout_secs
                    .transpose()?
                    .or(file.request_timeout_secs)
                    .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS),
            })?;
        let api_key = environment
            .api_key
            .or_else(|| file.api_key.filter(|value| !value.trim().is_empty()))
            .map(SecretString::from);
        let font_regular = choose_font_path(
            overrides.font_regular,
            environment.font_regular,
            file.font_regular,
        );
        let font_bold =
            choose_font_path(overrides.font_bold, environment.font_bold, file.font_bold);
        let font_fallback_regular = choose_font_path(
            overrides.font_fallback_regular,
            environment.font_fallback_regular,
            file.font_fallback_regular,
        );
        let font_fallback_bold = choose_font_path(
            overrides.font_fallback_bold,
            environment.font_fallback_bold,
            file.font_fallback_bold,
        );
        let layout_model = choose_layout_model_path(
            overrides.layout_model,
            environment.layout_model,
            file.layout_model,
        );
        let asset_mirror = choose_optional(
            overrides.asset_mirror,
            environment.asset_mirror,
            file.asset_mirror,
            "asset mirror",
        )?;
        let asset_cache_root = environment
            .cache_dir
            .or(file.cache_dir)
            .or_else(default_asset_cache_root)
            .ok_or_else(|| {
                MimusError::asset(
                    mimus_core::error::AssetReason::OutputFontUnavailable,
                    "could not determine the asset cache directory",
                )
                .with_hint("set MIMUS_CACHE_DIR or provide explicit font and layout-model paths")
            })?;
        let font_cache_dir = asset_cache_root.join("fonts/noto-serif-sc-2.001");
        let layout_model_cache_dir = asset_cache_root.join(format!(
            "models/pp-doclayoutv3-{}",
            crate::layout_assets::MODEL_COMMIT
        ));
        let user_glossary = overrides
            .glossary
            .as_deref()
            .map(Glossary::from_path)
            .transpose()?
            .unwrap_or_default();
        let cache_path = if overrides.no_cache {
            None
        } else {
            let path = overrides
                .cache
                .or(environment.cache_path)
                .or(file.cache)
                .map_or_else(default_cache_path, Ok)?;
            if path.as_os_str().is_empty() {
                return Err(MimusError::usage(
                    UsageReason::InvalidArguments,
                    "translation cache path must not be empty",
                ));
            }
            Some(path)
        };

        if backend == Backend::Openai && api_key.is_none() {
            return Err(MimusError::usage(
                UsageReason::InvalidArguments,
                "OpenAI API key is required",
            )
            .with_hint("set API_KEY or configure api_key in ~/.config/mimus/config.toml"));
        }

        Ok(Self {
            backend,
            base_url,
            model,
            target_language,
            font_regular,
            font_bold,
            font_fallback_regular,
            font_fallback_bold,
            layout_model,
            asset_mirror,
            font_cache_dir,
            layout_model_cache_dir,
            user_glossary,
            dump_glossary: overrides.dump_glossary,
            auto_terms: !overrides.no_auto_terms,
            cache_path,
            max_concurrency,
            request_timeout_secs,
            strict: overrides.strict,
            translate_table: overrides.translate_table,
            api_key,
        })
    }

    pub(crate) fn take_translator(&mut self) -> Result<Box<dyn Translator>> {
        match self.backend {
            Backend::None => Ok(Box::new(NoneTranslator)),
            Backend::Openai => Ok(Box::new(OpenAiTranslator::new(
                &self.base_url,
                self.model.clone(),
                self.api_key
                    .take()
                    .expect("validated OpenAI configuration has a key"),
                Duration::from_secs(self.request_timeout_secs),
            )?)),
        }
    }
}

impl ResolvedLayoutConfig {
    pub(crate) fn load(
        layout_model: Option<PathBuf>,
        asset_mirror: Option<String>,
    ) -> Result<Self> {
        load_dotenv_local()?;
        let file = read_file_config()?;
        let environment = EnvironmentConfig::read();
        let layout_model =
            choose_layout_model_path(layout_model, environment.layout_model, file.layout_model);
        let asset_mirror = choose_optional(
            asset_mirror,
            environment.asset_mirror,
            file.asset_mirror,
            "asset mirror",
        )?;
        let asset_cache_root = environment
            .cache_dir
            .or(file.cache_dir)
            .or_else(default_asset_cache_root)
            .ok_or_else(|| {
                MimusError::asset(
                    mimus_core::error::AssetReason::LayoutModelUnavailable,
                    "could not determine the layout-model cache directory",
                )
                .with_hint("set MIMUS_CACHE_DIR or provide --layout-model")
            })?;
        Ok(Self {
            layout_model,
            asset_mirror,
            layout_model_cache_dir: asset_cache_root.join(format!(
                "models/pp-doclayoutv3-{}",
                crate::layout_assets::MODEL_COMMIT
            )),
        })
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    backend: Option<Backend>,
    #[serde(alias = "endpoint")]
    base_url: Option<String>,
    #[serde(alias = "model_id")]
    model: Option<String>,
    target_language: Option<String>,
    api_key: Option<String>,
    font_regular: Option<PathBuf>,
    font_bold: Option<PathBuf>,
    font_fallback_regular: Option<PathBuf>,
    font_fallback_bold: Option<PathBuf>,
    layout_model: Option<PathBuf>,
    asset_mirror: Option<String>,
    cache_dir: Option<PathBuf>,
    cache: Option<PathBuf>,
    concurrency: Option<usize>,
    request_timeout_secs: Option<i64>,
}

struct EnvironmentConfig {
    backend: Option<Result<Backend>>,
    base_url: Option<String>,
    model: Option<String>,
    target_language: Option<String>,
    api_key: Option<String>,
    font_regular: Option<PathBuf>,
    font_bold: Option<PathBuf>,
    font_fallback_regular: Option<PathBuf>,
    font_fallback_bold: Option<PathBuf>,
    layout_model: Option<PathBuf>,
    asset_mirror: Option<String>,
    cache_dir: Option<PathBuf>,
    cache_path: Option<PathBuf>,
    concurrency: Option<Result<usize>>,
    request_timeout_secs: Option<Result<i64>>,
}

impl EnvironmentConfig {
    fn read() -> Self {
        Self {
            backend: first_env(&["MIMUS_BACKEND"]).map(|value| match value.as_str() {
                "openai" => Ok(Backend::Openai),
                "none" => Ok(Backend::None),
                _ => Err(MimusError::usage(
                    UsageReason::InvalidArguments,
                    "MIMUS_BACKEND must be openai or none",
                )),
            }),
            base_url: first_env(&["MIMUS_OPENAI_BASE_URL", "OPENAI_BASE_URL", "BASE_URL"]),
            model: first_env(&["MIMUS_OPENAI_MODEL", "OPENAI_MODEL", "MODEL_ID"]),
            target_language: first_env(&["MIMUS_TARGET_LANGUAGE", "TARGET_LANGUAGE"]),
            api_key: first_nonempty_env(&["MIMUS_OPENAI_API_KEY", "OPENAI_API_KEY", "API_KEY"]),
            font_regular: first_env(&["MIMUS_FONT_REGULAR"]).map(PathBuf::from),
            font_bold: first_env(&["MIMUS_FONT_BOLD"]).map(PathBuf::from),
            font_fallback_regular: first_env(&["MIMUS_FONT_FALLBACK_REGULAR"]).map(PathBuf::from),
            font_fallback_bold: first_env(&["MIMUS_FONT_FALLBACK_BOLD"]).map(PathBuf::from),
            layout_model: first_env(&["MIMUS_LAYOUT_MODEL"]).map(PathBuf::from),
            asset_mirror: first_env(&["MIMUS_ASSET_MIRROR"]),
            cache_dir: first_env(&["MIMUS_CACHE_DIR"]).map(PathBuf::from),
            cache_path: first_env(&["MIMUS_CACHE"])
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from),
            concurrency: first_env(&["MIMUS_CONCURRENCY"]).map(|value| {
                value.parse::<usize>().map_err(|_| {
                    MimusError::usage(
                        UsageReason::InvalidArguments,
                        "MIMUS_CONCURRENCY must be a positive integer",
                    )
                })
            }),
            request_timeout_secs: first_env(&["MIMUS_REQUEST_TIMEOUT"]).map(|value| {
                value.parse::<i64>().map_err(|_| {
                    MimusError::usage(
                        UsageReason::InvalidArguments,
                        "MIMUS_REQUEST_TIMEOUT must be an integer from 1 through 600 seconds",
                    )
                })
            }),
        }
    }
}

fn validate_request_timeout_secs(value: i64) -> Result<u64> {
    if !(1..=MAX_REQUEST_TIMEOUT_SECS).contains(&value) {
        return Err(MimusError::usage(
            UsageReason::InvalidArguments,
            "request timeout must be from 1 through 600 seconds",
        ));
    }
    Ok(value as u64)
}

fn choose_optional(
    flag: Option<String>,
    environment: Option<String>,
    file: Option<String>,
    name: &str,
) -> Result<Option<String>> {
    let value = flag.or(environment).or(file);
    if value.as_ref().is_some_and(|value| value.trim().is_empty()) {
        return Err(MimusError::usage(
            UsageReason::InvalidArguments,
            format!("{name} must not be empty"),
        ));
    }
    Ok(value)
}

fn default_asset_cache_root() -> Option<PathBuf> {
    if let Some(root) = first_env(&["XDG_CACHE_HOME"]) {
        return Some(PathBuf::from(root).join("mimus"));
    }
    let home = first_env(&["HOME"])?;
    let root = if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Caches/mimus")
    } else {
        PathBuf::from(home).join(".cache/mimus")
    };
    Some(root)
}

fn choose_font_path(
    flag: Option<PathBuf>,
    environment: Option<PathBuf>,
    file: Option<PathBuf>,
) -> Option<FontPathSelection> {
    flag.map(|path| FontPathSelection {
        path,
        source: "flag",
    })
    .or_else(|| {
        environment.map(|path| FontPathSelection {
            path,
            source: "environment",
        })
    })
    .or_else(|| {
        file.map(|path| FontPathSelection {
            path,
            source: "config",
        })
    })
}

fn choose_layout_model_path(
    flag: Option<PathBuf>,
    environment: Option<PathBuf>,
    file: Option<PathBuf>,
) -> Option<LayoutModelPathSelection> {
    flag.map(|path| LayoutModelPathSelection {
        path,
        source: "flag",
    })
    .or_else(|| {
        environment.map(|path| LayoutModelPathSelection {
            path,
            source: "environment",
        })
    })
    .or_else(|| {
        file.map(|path| LayoutModelPathSelection {
            path,
            source: "config",
        })
    })
}

fn default_cache_path() -> Result<PathBuf> {
    if let Some(root) = first_env(&["XDG_CACHE_HOME"]).filter(|root| !root.trim().is_empty()) {
        return Ok(PathBuf::from(root).join("mimus/translations.redb"));
    }
    first_env(&["HOME"])
        .filter(|root| !root.trim().is_empty())
        .map(|root| PathBuf::from(root).join(".cache/mimus/translations.redb"))
        .ok_or_else(|| {
            MimusError::usage(
                UsageReason::InvalidArguments,
                "could not resolve the default translation cache path",
            )
            .with_hint("set XDG_CACHE_HOME or HOME, pass --cache, or use --no-cache")
        })
}

fn choose(
    flag: Option<String>,
    environment: Option<String>,
    file: Option<String>,
    default: &str,
    name: &str,
) -> Result<String> {
    let value = flag
        .or(environment)
        .or(file)
        .unwrap_or_else(|| default.to_owned());
    if value.trim().is_empty() {
        return Err(MimusError::usage(
            UsageReason::InvalidArguments,
            format!("OpenAI {name} must not be empty"),
        ));
    }
    Ok(value)
}

fn first_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| std::env::var(name).ok())
}

fn first_nonempty_env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
}

fn load_dotenv_local() -> Result<()> {
    let path = Path::new(".env.local");
    if !path.exists() {
        return Ok(());
    }
    dotenvy::from_path(path)
        .map(|_| ())
        .map_err(|_| MimusError::usage(UsageReason::InvalidArguments, "could not parse .env.local"))
}

fn read_file_config() -> Result<FileConfig> {
    let Some(path) = config_path()? else {
        return Ok(FileConfig::default());
    };
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let contents = std::fs::read_to_string(&path).map_err(|_| {
        MimusError::usage(
            UsageReason::InvalidArguments,
            format!("could not read config file {}", path.display()),
        )
    })?;
    toml::from_str(&contents).map_err(|_| {
        MimusError::usage(
            UsageReason::InvalidArguments,
            format!("could not parse config file {}", path.display()),
        )
    })
}

fn config_path() -> Result<Option<PathBuf>> {
    if let Some(path) = first_env(&["MIMUS_CONFIG_FILE"]) {
        if path.trim().is_empty() {
            return Err(MimusError::usage(
                UsageReason::InvalidArguments,
                "MIMUS_CONFIG_FILE must not be empty",
            ));
        }
        return Ok(Some(PathBuf::from(path)));
    }
    if let Some(root) = first_env(&["XDG_CONFIG_HOME"]) {
        return Ok(Some(PathBuf::from(root).join("mimus/config.toml")));
    }
    Ok(first_env(&["HOME"]).map(|root| PathBuf::from(root).join(".config/mimus/config.toml")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_non_secret_field_uses_flag_then_environment_then_file() {
        assert_eq!(
            choose(
                Some("flag".to_owned()),
                Some("env".to_owned()),
                Some("file".to_owned()),
                "default",
                "test",
            )
            .unwrap(),
            "flag"
        );
        assert_eq!(
            choose(
                None,
                Some("env".to_owned()),
                Some("file".to_owned()),
                "default",
                "test",
            )
            .unwrap(),
            "env"
        );
        assert_eq!(
            choose(None, None, Some("file".to_owned()), "default", "test").unwrap(),
            "file"
        );
        assert_eq!(
            choose(None, None, None, "default", "test").unwrap(),
            "default"
        );
    }

    #[test]
    fn malformed_or_unknown_file_fields_are_rejected() {
        assert!(toml::from_str::<FileConfig>("model = [1]").is_err());
        assert!(toml::from_str::<FileConfig>("secret = 'value'").is_err());
    }

    #[test]
    fn request_timeout_accepts_only_the_documented_bounds() {
        assert_eq!(validate_request_timeout_secs(1).unwrap(), 1);
        assert_eq!(validate_request_timeout_secs(600).unwrap(), 600);
        for invalid in [-1, 0, 601] {
            assert!(validate_request_timeout_secs(invalid).is_err());
        }
    }

    #[test]
    fn layout_model_path_uses_flag_then_environment_then_config() {
        let chosen = choose_layout_model_path(
            Some(PathBuf::from("flag.onnx")),
            Some(PathBuf::from("env.onnx")),
            Some(PathBuf::from("config.onnx")),
        )
        .unwrap();
        assert_eq!(chosen.path, PathBuf::from("flag.onnx"));
        assert_eq!(chosen.source, "flag");

        let chosen = choose_layout_model_path(
            None,
            Some(PathBuf::from("env.onnx")),
            Some(PathBuf::from("config.onnx")),
        )
        .unwrap();
        assert_eq!(chosen.path, PathBuf::from("env.onnx"));
        assert_eq!(chosen.source, "environment");

        let chosen =
            choose_layout_model_path(None, None, Some(PathBuf::from("config.onnx"))).unwrap();
        assert_eq!(chosen.path, PathBuf::from("config.onnx"));
        assert_eq!(chosen.source, "config");
    }
}
