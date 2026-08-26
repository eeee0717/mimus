use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ValueEnum;
use mimus_core::error::{MimusError, Result, UsageReason};
use mimus_core::translate::{NoneTranslator, OpenAiTranslator, Translator};
use secrecy::SecretString;
use serde::Deserialize;

const DEFAULT_BASE_URL: &str = "https://api.openai.com";
const DEFAULT_MODEL: &str = "gpt-4.1-mini";
const DEFAULT_TARGET_LANGUAGE: &str = "zh-CN";

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
    pub asset_mirror: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct FontPathSelection {
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
    pub asset_mirror: Option<String>,
    pub font_cache_dir: PathBuf,
    api_key: Option<SecretString>,
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
        let asset_mirror = choose_optional(
            overrides.asset_mirror,
            environment.asset_mirror,
            file.asset_mirror,
            "asset mirror",
        )?;
        let font_cache_dir = environment
            .cache_dir
            .or(file.cache_dir)
            .map(|root| root.join("fonts/noto-sans-sc-2.004"))
            .or_else(default_cache_dir)
            .ok_or_else(|| {
                MimusError::asset(
                    mimus_core::error::AssetReason::OutputFontUnavailable,
                    "could not determine the output-font cache directory",
                )
                .with_hint("set MIMUS_CACHE_DIR or provide --font and --font-bold")
            })?;

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
            asset_mirror,
            font_cache_dir,
            api_key,
        })
    }

    pub(crate) fn into_translator(self) -> Result<Box<dyn Translator>> {
        match self.backend {
            Backend::None => Ok(Box::new(NoneTranslator)),
            Backend::Openai => Ok(Box::new(OpenAiTranslator::new(
                &self.base_url,
                self.model,
                self.api_key
                    .expect("validated OpenAI configuration has a key"),
                Duration::from_secs(30),
            )?)),
        }
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
    asset_mirror: Option<String>,
    cache_dir: Option<PathBuf>,
}

struct EnvironmentConfig {
    backend: Option<Result<Backend>>,
    base_url: Option<String>,
    model: Option<String>,
    target_language: Option<String>,
    api_key: Option<String>,
    font_regular: Option<PathBuf>,
    font_bold: Option<PathBuf>,
    asset_mirror: Option<String>,
    cache_dir: Option<PathBuf>,
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
            asset_mirror: first_env(&["MIMUS_ASSET_MIRROR"]),
            cache_dir: first_env(&["MIMUS_CACHE_DIR"]).map(PathBuf::from),
        }
    }
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

fn default_cache_dir() -> Option<PathBuf> {
    if let Some(root) = first_env(&["XDG_CACHE_HOME"]) {
        return Some(PathBuf::from(root).join("mimus/fonts/noto-sans-sc-2.004"));
    }
    let home = first_env(&["HOME"])?;
    let root = if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Caches/mimus")
    } else {
        PathBuf::from(home).join(".cache/mimus")
    };
    Some(root.join("fonts/noto-sans-sc-2.004"))
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
}
