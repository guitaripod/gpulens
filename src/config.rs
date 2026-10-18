//! Persisted tool settings: defaults, validation and atomic saves.

use std::fs;
use std::io::{ErrorKind, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::Duration;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const APP_QUALIFIER: &str = "";
const APP_ORGANIZATION: &str = "";
const APP_NAME: &str = "gpulens";
const CONFIG_FILE_NAME: &str = "config.toml";
const FALLBACK_FILE_NAME: &str = "gpulens.toml";
const INTERVAL_MS: RangeInclusive<u64> = 250..=10_000;
const WINDOW_SECS: RangeInclusive<u64> = 30..=86_400;
const HISTORY_CAP_SECS: u64 = 86_400;
const LLM_TIMEOUT_SECS: RangeInclusive<u64> = 1..=300;
const NARRATE_COOLDOWN_SECS: RangeInclusive<u64> = 5..=3_600;

/// Tool settings. A missing key takes its default, an unknown key is rejected.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub interval_ms: u64,
    pub history_secs: u64,
    pub window_secs: u64,
    pub ascii: bool,
    pub allow_kill: bool,
    pub calm: bool,
    pub animate: bool,
    pub llm: LlmConfig,
}

/// Settings for the local LLM that classifies and narrates processes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    pub enabled: bool,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub timeout_secs: u64,
    pub classify: bool,
    pub narrate: bool,
    pub narrate_cooldown_secs: u64,
}

/// Why a config could not be read, parsed, validated or written.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid setting: {0}")]
    Invalid(String),
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            history_secs: 3600,
            window_secs: 300,
            ascii: false,
            allow_kill: true,
            calm: true,
            animate: true,
            llm: LlmConfig::default(),
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: None,
            model: None,
            api_key_env: None,
            timeout_secs: 30,
            classify: true,
            narrate: false,
            narrate_cooldown_secs: 20,
        }
    }
}

impl Config {
    /// `config.toml` in the platform config directory, or `./gpulens.toml` when no home directory is known.
    pub fn default_path() -> PathBuf {
        project_dirs()
            .map(|dirs| dirs.config_dir().join(CONFIG_FILE_NAME))
            .unwrap_or_else(|| PathBuf::from(FALLBACK_FILE_NAME))
    }

    /// Reads and validates the file. A missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Config::default()),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let config: Config = toml::from_str(&text).map_err(|err| ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Validates, then writes pretty TOML atomically: a temp file in the same directory is renamed over the target.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        let text =
            toml::to_string_pretty(self).map_err(|err| ConfigError::Invalid(err.to_string()))?;
        write_atomically(path, &text)
    }

    /// Checks every setting against its allowed range and names the offending key on failure.
    pub fn validate(&self) -> Result<(), ConfigError> {
        check_range("interval_ms", self.interval_ms, &INTERVAL_MS)?;
        check_range("window_secs", self.window_secs, &WINDOW_SECS)?;
        check_history(self.history_secs, self.window_secs)?;
        check_range("llm.timeout_secs", self.llm.timeout_secs, &LLM_TIMEOUT_SECS)?;
        check_range(
            "llm.narrate_cooldown_secs",
            self.llm.narrate_cooldown_secs,
            &NARRATE_COOLDOWN_SECS,
        )?;
        check_base_url(self.llm.base_url.as_deref())
    }

    /// Time between two samples.
    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }

    /// How far back the history keeps samples.
    pub fn history(&self) -> Duration {
        Duration::from_secs(self.history_secs)
    }

    /// Length of the window used for rolling averages.
    pub fn window(&self) -> Duration {
        Duration::from_secs(self.window_secs)
    }
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from(APP_QUALIFIER, APP_ORGANIZATION, APP_NAME)
}

fn check_range(key: &str, value: u64, allowed: &RangeInclusive<u64>) -> Result<(), ConfigError> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "{key} must be between {} and {}, got {value}",
            allowed.start(),
            allowed.end()
        )))
    }
}

/// History must cover at least one window and never more than a day.
fn check_history(history: u64, window: u64) -> Result<(), ConfigError> {
    check_range("history_secs", history, &(window..=HISTORY_CAP_SECS))
}

fn check_base_url(base_url: Option<&str>) -> Result<(), ConfigError> {
    match base_url {
        Some(url) if !has_http_scheme(url) => Err(ConfigError::Invalid(format!(
            "llm.base_url must start with http:// or https://, got \"{url}\""
        ))),
        _ => Ok(()),
    }
}

fn has_http_scheme(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn write_atomically(path: &Path, text: &str) -> Result<(), ConfigError> {
    let fail = |source: std::io::Error| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    let temp = temp_path_for(path);
    let outcome = write_private_file(&temp, text).and_then(|()| fs::rename(&temp, path));
    outcome.map_err(|source| {
        let _ = fs::remove_file(&temp);
        fail(source)
    })
}

/// The temp name sits next to the target so the final rename never crosses filesystems.
fn temp_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(CONFIG_FILE_NAME);
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

fn write_private_file(path: &Path, text: &str) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    restrict_to_owner(&mut options);
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// The file may name an API-key environment variable, so only the owner may read it.
#[cfg(unix)]
fn restrict_to_owner(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn restrict_to_owner(_options: &mut fs::OpenOptions) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{TempDir, tempdir};

    fn sandbox() -> (TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        (dir, path)
    }

    fn put(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
    }

    fn invalid_message(config: &Config) -> String {
        match config.validate() {
            Err(ConfigError::Invalid(message)) => message,
            other => panic!("expected an Invalid error, got {other:?}"),
        }
    }

    fn assert_rejects(config: &Config, key: &str, value: u64) {
        let message = invalid_message(config);
        assert!(message.contains(key), "{message}");
        assert!(message.contains(&format!("got {value}")), "{message}");
    }

    fn parse_message(path: &Path) -> String {
        match Config::load(path) {
            Err(ConfigError::Parse { message, .. }) => message,
            other => panic!("expected a Parse error, got {other:?}"),
        }
    }

    #[test]
    fn defaults_have_documented_values_and_durations() {
        let config = Config::default();
        assert_eq!(config.interval_ms, 1000);
        assert_eq!(config.history_secs, 3600);
        assert_eq!(config.window_secs, 300);
        assert!(!config.ascii);
        assert!(config.allow_kill);
        assert!(config.llm.enabled && config.llm.classify && !config.llm.narrate);
        assert_eq!(config.llm.timeout_secs, 30);
        assert_eq!(config.llm.narrate_cooldown_secs, 20);
        assert_eq!(config.interval(), Duration::from_secs(1));
        assert_eq!(config.history(), Duration::from_secs(3600));
        assert_eq!(config.window(), Duration::from_secs(300));
    }

    #[test]
    fn defaults_round_trip_through_save_and_load() {
        let (_dir, path) = sandbox();
        Config::default().save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }

    #[test]
    fn customised_values_round_trip() {
        let (_dir, path) = sandbox();
        let config = Config {
            interval_ms: 500,
            ascii: true,
            allow_kill: false,
            llm: LlmConfig {
                base_url: Some("http://localhost:11434/v1".into()),
                model: Some("qwen3-4b".into()),
                api_key_env: Some("LLM_KEY".into()),
                ..LlmConfig::default()
            },
            ..Config::default()
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn save_replaces_an_existing_file() {
        let (_dir, path) = sandbox();
        let mut config = Config::default();
        config.save(&path).unwrap();
        config.window_secs = 600;
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap().window_secs, 600);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let (_dir, path) = sandbox();
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let (_dir, path) = sandbox();
        put(&path, "interval_ms = 500\n\n[llm]\nclassify = false\n");
        let config = Config::load(&path).unwrap();
        assert_eq!(config.interval_ms, 500);
        assert!(!config.llm.classify);
        assert_eq!(config.history_secs, 3600);
        assert!(!config.llm.narrate);
        assert_eq!(config.llm.timeout_secs, 30);
    }

    #[test]
    fn unknown_top_level_key_is_a_parse_error_naming_it() {
        let (_dir, path) = sandbox();
        put(&path, "intervall_ms = 500\n");
        assert!(parse_message(&path).contains("intervall_ms"));
    }

    #[test]
    fn unknown_llm_key_is_a_parse_error_naming_it() {
        let (_dir, path) = sandbox();
        put(&path, "[llm]\nnarate = true\n");
        assert!(parse_message(&path).contains("narate"));
    }

    #[test]
    fn type_mismatch_is_a_parse_error_with_line_and_column() {
        let (_dir, path) = sandbox();
        put(&path, "interval_ms = \"fast\"\n");
        let message = parse_message(&path);
        assert!(message.contains("interval_ms"), "{message}");
        assert!(message.contains("line 1"), "{message}");
    }

    #[test]
    fn load_runs_validation() {
        let (_dir, path) = sandbox();
        put(&path, "interval_ms = 1\n");
        assert!(
            matches!(Config::load(&path), Err(ConfigError::Invalid(message)) if message.contains("interval_ms"))
        );
    }

    #[test]
    fn reading_a_directory_is_a_read_error() {
        let dir = tempdir().unwrap();
        assert!(matches!(
            Config::load(dir.path()),
            Err(ConfigError::Read { .. })
        ));
    }

    #[test]
    fn interval_bounds_are_enforced() {
        let mut config = Config {
            interval_ms: 249,
            ..Config::default()
        };
        assert_rejects(&config, "interval_ms", 249);
        config.interval_ms = 250;
        assert!(config.validate().is_ok());
        config.interval_ms = 10_000;
        assert!(config.validate().is_ok());
        config.interval_ms = 10_001;
        assert_rejects(&config, "interval_ms", 10_001);
    }

    #[test]
    fn window_bounds_are_enforced() {
        let mut config = Config {
            window_secs: 29,
            ..Config::default()
        };
        assert_rejects(&config, "window_secs", 29);
        config.window_secs = 30;
        config.history_secs = 30;
        assert!(config.validate().is_ok());
        config.window_secs = 86_400;
        config.history_secs = 86_400;
        assert!(config.validate().is_ok());
        config.window_secs = 86_401;
        assert_rejects(&config, "window_secs", 86_401);
    }

    #[test]
    fn history_must_cover_the_window_and_stay_within_a_day() {
        let mut config = Config {
            window_secs: 300,
            history_secs: 299,
            ..Config::default()
        };
        assert_rejects(&config, "history_secs", 299);
        config.history_secs = 300;
        assert!(config.validate().is_ok());
        config.history_secs = 86_400;
        assert!(config.validate().is_ok());
        config.history_secs = 86_401;
        assert_rejects(&config, "history_secs", 86_401);
    }

    #[test]
    fn llm_timeout_bounds_are_enforced() {
        let mut config = Config::default();
        config.llm.timeout_secs = 0;
        assert_rejects(&config, "llm.timeout_secs", 0);
        config.llm.timeout_secs = 1;
        assert!(config.validate().is_ok());
        config.llm.timeout_secs = 300;
        assert!(config.validate().is_ok());
        config.llm.timeout_secs = 301;
        assert_rejects(&config, "llm.timeout_secs", 301);
    }

    #[test]
    fn narrate_cooldown_bounds_are_enforced() {
        let mut config = Config::default();
        config.llm.narrate_cooldown_secs = 4;
        assert_rejects(&config, "llm.narrate_cooldown_secs", 4);
        config.llm.narrate_cooldown_secs = 5;
        assert!(config.validate().is_ok());
        config.llm.narrate_cooldown_secs = 3_600;
        assert!(config.validate().is_ok());
        config.llm.narrate_cooldown_secs = 3_601;
        assert_rejects(&config, "llm.narrate_cooldown_secs", 3_601);
    }

    #[test]
    fn base_url_must_be_http_or_https() {
        let mut config = Config::default();
        assert!(config.validate().is_ok());
        for url in ["localhost:8080", "", "ftp://example.com"] {
            config.llm.base_url = Some(url.into());
            let message = invalid_message(&config);
            assert!(message.contains("llm.base_url"), "{message}");
            assert!(message.contains(url), "{message}");
        }
        config.llm.base_url = Some("http://localhost:11434/v1".into());
        assert!(config.validate().is_ok());
        config.llm.base_url = Some("https://example.com".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn atomic_save_leaves_no_temp_file_behind() {
        let (dir, path) = sandbox();
        Config::default().save(&path).unwrap();
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["config.toml".to_string()]);
    }

    #[test]
    fn save_creates_nested_directories() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("config.toml");
        Config::default().save(&path).unwrap();
        assert!(path.is_file());
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }

    #[test]
    fn save_refuses_an_invalid_config_and_writes_nothing() {
        let (_dir, path) = sandbox();
        let config = Config {
            interval_ms: 1,
            ..Config::default()
        };
        assert!(matches!(config.save(&path), Err(ConfigError::Invalid(_))));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_readable_only_by_owner() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = sandbox();
        Config::default().save(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn default_path_is_a_config_toml_or_the_fallback() {
        let path = Config::default_path();
        let name = path.file_name().unwrap();
        assert!(name == "config.toml" || name == "gpulens.toml", "{path:?}");
    }
}
