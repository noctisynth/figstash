//! Configuration resolution with CLI > environment > file > default precedence.

use crate::args::Cli;
use directories::ProjectDirs;
use figstash_core::{AppError, AppResult, ErrorCode};
use serde::Deserialize;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_CONNECT_TIMEOUT_SECONDS: u64 = 10;
const DEFAULT_TOTAL_TIMEOUT_SECONDS: u64 = 120;
const DEFAULT_MAXIMUM_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    data_dir: Option<PathBuf>,
    connect_timeout_seconds: Option<u64>,
    total_timeout_seconds: Option<u64>,
    inherit_proxy: Option<bool>,
    maximum_download_bytes: Option<u64>,
    log_level: Option<String>,
    stale_after_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    pub(crate) data_dir: PathBuf,
    pub(crate) config_dir: PathBuf,
    pub(crate) connect_timeout: Duration,
    pub(crate) total_timeout: Duration,
    pub(crate) inherit_proxy: bool,
    pub(crate) maximum_download_bytes: u64,
    pub(crate) log_level: String,
    pub(crate) stale_after: Option<Duration>,
}

impl AppConfig {
    pub(crate) fn load(cli: &Cli) -> AppResult<Self> {
        let project = ProjectDirs::from("dev", "Figstash", "Figstash").ok_or_else(|| {
            AppError::new(
                ErrorCode::StoreFailed,
                "The operating system did not provide standard application directories.",
            )
        })?;
        let config_dir = cli
            .config_dir
            .clone()
            .or_else(|| env_path("FIGSTASH_CONFIG_DIR"))
            .unwrap_or_else(|| project.config_dir().to_path_buf());
        let file = load_file(&config_dir.join("config.toml"))?;
        let data_dir = cli
            .data_dir
            .clone()
            .or_else(|| env_path("FIGSTASH_DATA_DIR"))
            .or(file.data_dir)
            .unwrap_or_else(|| project.data_local_dir().to_path_buf());
        let connect_timeout_seconds = env_integer("FIGSTASH_CONNECT_TIMEOUT_SECONDS")?
            .or(file.connect_timeout_seconds)
            .unwrap_or(DEFAULT_CONNECT_TIMEOUT_SECONDS);
        let total_timeout_seconds = env_integer("FIGSTASH_TOTAL_TIMEOUT_SECONDS")?
            .or(file.total_timeout_seconds)
            .unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECONDS);
        let inherit_proxy = env_boolean("FIGSTASH_INHERIT_PROXY")?
            .or(file.inherit_proxy)
            .unwrap_or(true);
        let maximum_download_bytes = env_integer("FIGSTASH_MAXIMUM_DOWNLOAD_BYTES")?
            .or(file.maximum_download_bytes)
            .unwrap_or(DEFAULT_MAXIMUM_DOWNLOAD_BYTES);
        let log_level = cli
            .log_level
            .map(|level| level.as_str().to_owned())
            .or_else(|| std::env::var("FIGSTASH_LOG_LEVEL").ok())
            .or(file.log_level)
            .unwrap_or_else(|| "warn".to_owned());
        validate_log_level(&log_level)?;
        Ok(Self {
            data_dir,
            config_dir,
            connect_timeout: Duration::from_secs(connect_timeout_seconds),
            total_timeout: Duration::from_secs(total_timeout_seconds),
            inherit_proxy,
            maximum_download_bytes,
            log_level,
            stale_after: file.stale_after_seconds.map(Duration::from_secs),
        })
    }
}

fn load_file(path: &Path) -> AppResult<FileConfig> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileConfig::default());
        }
        Err(error) => {
            return Err(AppError::new(
                ErrorCode::StoreFailed,
                "Failed to read the Figstash configuration file.",
            )
            .with_details(json!({"path": path, "reason": error.to_string()})));
        }
    };
    toml::from_str(&contents).map_err(|error| {
        AppError::new(
            ErrorCode::InvalidInput,
            "The Figstash configuration file is invalid.",
        )
        .with_details(json!({"path": path, "reason": error.to_string()}))
    })
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_integer(name: &str) -> AppResult<Option<u64>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    value.parse::<u64>().map(Some).map_err(|error| {
        AppError::new(
            ErrorCode::InvalidInput,
            format!("Environment variable `{name}` must be a non-negative integer."),
        )
        .with_detail("reason", json!(error.to_string()))
    })
}

fn env_boolean(name: &str) -> AppResult<Option<bool>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    match value.to_string_lossy().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(Some(true)),
        "0" | "false" | "no" => Ok(Some(false)),
        _ => Err(AppError::new(
            ErrorCode::InvalidInput,
            format!("Environment variable `{name}` must be a boolean."),
        )),
    }
}

fn validate_log_level(level: &str) -> AppResult<()> {
    if matches!(level, "off" | "error" | "warn" | "info" | "debug" | "trace") {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::InvalidInput,
            "The configured log level is invalid.",
        )
        .with_detail("logLevel", json!(level)))
    }
}
