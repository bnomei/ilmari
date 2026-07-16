//! Strongly typed, optional TOML configuration for Ilmari.
//!
//! Loading retains which view fields were explicitly configured so callers can
//! resolve each field independently against CLI and remembered-state values.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::colors::Palette;
use crate::view_state::ViewState;

pub const DEFAULT_REFRESH_SECONDS: u64 = 5;
pub const DEFAULT_PROCESS_REFRESH_SECONDS: u64 = 15;
pub const DEFAULT_MCP_PORT: u16 = 62_778;

/// Effective application configuration after built-in defaults and TOML are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub runtime: RuntimeConfig,
    pub scanner: ScannerConfig,
    pub tui: TuiConfig,
    pub palette: Palette,
    pub socket: SocketConfig,
    pub mcp: McpConfig,
    pub view: ViewConfig,
    pub badges: RendererConfig,
    pub status: RendererConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub refresh_seconds: u64,
    pub process_refresh_seconds: u64,
}

impl RuntimeConfig {
    pub fn refresh_interval(&self) -> Duration {
        Duration::from_secs(self.refresh_seconds)
    }

    pub fn process_refresh_interval(&self) -> Duration {
        Duration::from_secs(self.process_refresh_seconds)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannerConfig {
    pub git: bool,
    pub output_tail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiConfig {
    pub enabled: bool,
    pub bell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketConfig {
    pub enabled: bool,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfig {
    pub enabled: bool,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewConfig {
    pub app: bool,
    pub git: bool,
    pub detail: bool,
    pub time: bool,
    pub output: bool,
    pub stats: bool,
    pub remember: bool,
}

impl ViewConfig {
    pub fn values(&self) -> ViewState {
        ViewState {
            app: self.app,
            git: self.git,
            detail: self.detail,
            time: self.time,
            output: self.output,
            stats: self.stats,
        }
    }
}

/// Symbol and tmux style used for one agent state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFormat {
    pub symbol: String,
    pub style: String,
}

/// Effective configuration for a user-placeable tmux renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererConfig {
    pub enabled: bool,
    pub separator: String,
    pub running: StateFormat,
    pub waiting_input: StateFormat,
    pub finished: StateFormat,
}

/// Optional one-run view overrides, normally populated by the CLI parser.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewOverrides {
    pub app: Option<bool>,
    pub git: Option<bool>,
    pub detail: Option<bool>,
    pub time: Option<bool>,
    pub output: Option<bool>,
    pub stats: Option<bool>,
}

/// Whether each resolved view is protected from responsive default changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewPins {
    pub app: bool,
    pub git: bool,
    pub detail: bool,
    pub time: bool,
    pub output: bool,
    pub stats: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedViews {
    pub values: ViewState,
    pub pinned: ViewPins,
    pub remember: bool,
}

impl Default for ResolvedViews {
    fn default() -> Self {
        Self { values: ViewConfig::default().values(), pinned: ViewPins::default(), remember: true }
    }
}

/// A loaded config plus the metadata needed for field-aware view resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub values: Config,
    pub path: Option<PathBuf>,
    pub loaded_from_file: bool,
    explicit_views: ViewOverrides,
}

impl LoadedConfig {
    /// Load using an explicit environment map. This does not consult `ILMARI_*` variables.
    pub fn load_from_env_map(env: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        match config_path_from_env(env) {
            Some(path) => Self::load_from_path(path),
            None => Ok(Self::defaults(None)),
        }
    }

    /// Load a specific file. A missing file is equivalent to built-in defaults.
    pub fn load_from_path(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::defaults(Some(path)));
            }
            Err(source) => return Err(ConfigError::Read { path, source }),
        };

        let raw: RawConfig = toml::from_str(&source)
            .map_err(|source| ConfigError::Parse { path: path.clone(), source })?;
        Self::from_raw(raw, path)
    }

    #[cfg(test)]
    fn explicit_views(&self) -> ViewOverrides {
        self.explicit_views
    }

    /// Resolve views independently as CLI, explicit TOML, remembered state, then built-in.
    pub fn resolve_views(
        &self,
        cli: ViewOverrides,
        remembered: Option<&ViewState>,
    ) -> ResolvedViews {
        let remembered = self.values.view.remember.then_some(remembered).flatten();
        let defaults = ViewConfig::default().values();

        let (app, app_pinned) = resolve_view(
            cli.app,
            self.explicit_views.app,
            remembered.map(|state| state.app),
            defaults.app,
        );
        let (git, git_pinned) = resolve_view(
            cli.git,
            self.explicit_views.git,
            remembered.map(|state| state.git),
            defaults.git,
        );
        let (detail, detail_pinned) = resolve_view(
            cli.detail,
            self.explicit_views.detail,
            remembered.map(|state| state.detail),
            defaults.detail,
        );
        let (time, time_pinned) = resolve_view(
            cli.time,
            self.explicit_views.time,
            remembered.map(|state| state.time),
            defaults.time,
        );
        let (output, output_pinned) = resolve_view(
            cli.output,
            self.explicit_views.output,
            remembered.map(|state| state.output),
            defaults.output,
        );
        let (stats, stats_pinned) = resolve_view(
            cli.stats,
            self.explicit_views.stats,
            remembered.map(|state| state.stats),
            defaults.stats,
        );

        ResolvedViews {
            values: ViewState { app, git, detail, time, output, stats },
            pinned: ViewPins {
                app: app_pinned,
                git: git_pinned,
                detail: detail_pinned,
                time: time_pinned,
                output: output_pinned,
                stats: stats_pinned,
            },
            remember: self.values.view.remember,
        }
    }

    fn defaults(path: Option<PathBuf>) -> Self {
        Self {
            values: Config::default(),
            path,
            loaded_from_file: false,
            explicit_views: ViewOverrides::default(),
        }
    }

    fn from_raw(raw: RawConfig, path: PathBuf) -> Result<Self, ConfigError> {
        validate_positive(&path, "runtime.refresh_seconds", raw.runtime.refresh_seconds)?;
        validate_positive(
            &path,
            "runtime.process_refresh_seconds",
            raw.runtime.process_refresh_seconds,
        )?;

        let palette =
            match raw.palette.colors.as_deref() {
                Some(colors) => Palette::from_csv(colors).map_err(|message| {
                    ConfigError::Invalid { path: path.clone(), field: "palette.colors", message }
                })?,
                None => Palette::default(),
            };
        let explicit_views = raw.view.overrides();
        let mut values = Config::default();

        values.runtime.refresh_seconds =
            raw.runtime.refresh_seconds.unwrap_or(DEFAULT_REFRESH_SECONDS);
        values.runtime.process_refresh_seconds =
            raw.runtime.process_refresh_seconds.unwrap_or(DEFAULT_PROCESS_REFRESH_SECONDS);
        values.scanner.git = raw.scanner.git.unwrap_or(values.scanner.git);
        values.scanner.output_tail = raw.scanner.output_tail.unwrap_or(values.scanner.output_tail);
        values.tui.enabled = raw.tui.enabled.unwrap_or(values.tui.enabled);
        values.tui.bell = raw.tui.bell.unwrap_or(values.tui.bell);
        values.palette = palette;
        values.socket.enabled = raw.socket.enabled.unwrap_or(raw.socket.path.is_some());
        values.socket.path = raw.socket.path;
        values.mcp.enabled = raw.mcp.enabled.unwrap_or(raw.mcp.port.is_some());
        values.mcp.port = raw.mcp.port.unwrap_or(DEFAULT_MCP_PORT);
        values.view = raw.view.merge(ViewConfig::default());
        values.badges = raw.badges.merge(RendererConfig::badges_default());
        values.status = raw.status.merge(RendererConfig::status_default());

        Ok(Self { values, path: Some(path), loaded_from_file: true, explicit_views })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runtime: RuntimeConfig {
                refresh_seconds: DEFAULT_REFRESH_SECONDS,
                process_refresh_seconds: DEFAULT_PROCESS_REFRESH_SECONDS,
            },
            scanner: ScannerConfig { git: true, output_tail: true },
            tui: TuiConfig { enabled: cfg!(feature = "tui"), bell: true },
            palette: Palette::default(),
            socket: SocketConfig { enabled: false, path: None },
            mcp: McpConfig { enabled: false, port: DEFAULT_MCP_PORT },
            view: ViewConfig::default(),
            badges: RendererConfig::badges_default(),
            status: RendererConfig::status_default(),
        }
    }
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            app: false,
            git: true,
            detail: false,
            time: true,
            output: true,
            stats: false,
            remember: true,
        }
    }
}

impl RendererConfig {
    fn badges_default() -> Self {
        Self {
            enabled: true,
            separator: " ".to_string(),
            running: state_format("●", "fg=blue"),
            waiting_input: state_format("?", "fg=yellow"),
            finished: state_format("✓", "fg=green"),
        }
    }

    fn status_default() -> Self {
        Self {
            enabled: true,
            separator: " ".to_string(),
            running: state_format("R", "fg=blue"),
            waiting_input: state_format("I", "fg=yellow"),
            finished: state_format("F", "fg=green"),
        }
    }
}

fn state_format(symbol: &str, style: &str) -> StateFormat {
    StateFormat { symbol: symbol.to_string(), style: style.to_string() }
}

fn resolve_view(
    cli: Option<bool>,
    configured: Option<bool>,
    remembered: Option<bool>,
    default: bool,
) -> (bool, bool) {
    if let Some(value) = cli {
        (value, true)
    } else if let Some(value) = configured {
        (value, true)
    } else if let Some(value) = remembered {
        (value, true)
    } else {
        (default, false)
    }
}

fn validate_positive(
    path: &Path,
    field: &'static str,
    value: Option<u64>,
) -> Result<(), ConfigError> {
    if value == Some(0) {
        return Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            field,
            message: "must be a positive integer number of seconds".to_string(),
        });
    }
    Ok(())
}

/// Resolve `$XDG_CONFIG_HOME/ilmari/config.toml`, then `~/.config/ilmari/config.toml`.
pub fn config_path_from_env(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    nonempty_path(env.get("XDG_CONFIG_HOME").map(String::as_str))
        .map(|base| base.join("ilmari").join("config.toml"))
        .or_else(|| {
            nonempty_path(env.get("HOME").map(String::as_str))
                .map(|home| home.join(".config").join("ilmari").join("config.toml"))
        })
}

fn nonempty_path(value: Option<&str>) -> Option<PathBuf> {
    value.filter(|value| !value.trim().is_empty()).map(PathBuf::from)
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read Ilmari config at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid Ilmari config at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid Ilmari config at {path}: `{field}` {message}")]
    Invalid { path: PathBuf, field: &'static str, message: String },
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    runtime: RawRuntimeConfig,
    scanner: RawScannerConfig,
    tui: RawTuiConfig,
    palette: RawPaletteConfig,
    socket: RawSocketConfig,
    mcp: RawMcpConfig,
    view: RawViewConfig,
    badges: RawRendererConfig,
    status: RawRendererConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRuntimeConfig {
    refresh_seconds: Option<u64>,
    process_refresh_seconds: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawScannerConfig {
    git: Option<bool>,
    output_tail: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawTuiConfig {
    enabled: Option<bool>,
    bell: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawPaletteConfig {
    colors: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawSocketConfig {
    enabled: Option<bool>,
    path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawMcpConfig {
    enabled: Option<bool>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawViewConfig {
    app: Option<bool>,
    git: Option<bool>,
    detail: Option<bool>,
    time: Option<bool>,
    output: Option<bool>,
    stats: Option<bool>,
    remember: Option<bool>,
}

impl RawViewConfig {
    fn overrides(&self) -> ViewOverrides {
        ViewOverrides {
            app: self.app,
            git: self.git,
            detail: self.detail,
            time: self.time,
            output: self.output,
            stats: self.stats,
        }
    }

    fn merge(self, defaults: ViewConfig) -> ViewConfig {
        ViewConfig {
            app: self.app.unwrap_or(defaults.app),
            git: self.git.unwrap_or(defaults.git),
            detail: self.detail.unwrap_or(defaults.detail),
            time: self.time.unwrap_or(defaults.time),
            output: self.output.unwrap_or(defaults.output),
            stats: self.stats.unwrap_or(defaults.stats),
            remember: self.remember.unwrap_or(defaults.remember),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRendererConfig {
    enabled: Option<bool>,
    separator: Option<String>,
    running: RawStateFormat,
    waiting_input: RawStateFormat,
    finished: RawStateFormat,
}

impl RawRendererConfig {
    fn merge(self, defaults: RendererConfig) -> RendererConfig {
        RendererConfig {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            separator: self.separator.unwrap_or(defaults.separator),
            running: self.running.merge(defaults.running),
            waiting_input: self.waiting_input.merge(defaults.waiting_input),
            finished: self.finished.merge(defaults.finished),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawStateFormat {
    symbol: Option<String>,
    style: Option<String>,
}

impl RawStateFormat {
    fn merge(self, defaults: StateFormat) -> StateFormat {
        StateFormat {
            symbol: self.symbol.unwrap_or(defaults.symbol),
            style: self.style.unwrap_or(defaults.style),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ilmari-config-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn write_config(label: &str, source: &str) -> PathBuf {
        let dir = test_dir(label);
        fs::create_dir_all(&dir).expect("test config directory");
        let path = dir.join("config.toml");
        fs::write(&path, source).expect("write test config");
        path
    }

    #[test]
    fn xdg_path_wins_and_home_is_the_fallback() {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), "/home/tester".to_string());
        assert_eq!(
            config_path_from_env(&env),
            Some(PathBuf::from("/home/tester/.config/ilmari/config.toml"))
        );

        env.insert("XDG_CONFIG_HOME".to_string(), "/cfg".to_string());
        assert_eq!(config_path_from_env(&env), Some(PathBuf::from("/cfg/ilmari/config.toml")));
    }

    #[test]
    fn missing_file_uses_complete_built_in_defaults() {
        let path = test_dir("missing").join("config.toml");
        let loaded = LoadedConfig::load_from_path(&path).expect("missing config is normal");

        assert!(!loaded.loaded_from_file);
        assert_eq!(loaded.path.as_deref(), Some(path.as_path()));
        assert_eq!(loaded.values.runtime.refresh_seconds, DEFAULT_REFRESH_SECONDS);
        assert!(loaded.values.scanner.git);
        assert!(loaded.values.scanner.output_tail);
        assert!(loaded.values.view.remember);
        assert!(loaded.values.badges.enabled);
        assert!(loaded.values.status.enabled);
        assert_eq!(loaded.explicit_views(), ViewOverrides::default());
    }

    #[test]
    fn non_secret_ilmari_environment_variables_are_not_configuration() {
        let mut env = BTreeMap::new();
        env.insert("ILMARI_REFRESH_SECONDS".to_string(), "999".to_string());
        env.insert("ILMARI_TUI".to_string(), "false".to_string());
        env.insert("ILMARI_SOCKET".to_string(), "true".to_string());

        let loaded = LoadedConfig::load_from_env_map(&env).expect("defaults load");
        assert_eq!(loaded.values.runtime.refresh_seconds, DEFAULT_REFRESH_SECONDS);
        assert_eq!(loaded.values.tui.enabled, cfg!(feature = "tui"));
        assert!(!loaded.values.socket.enabled);
    }

    #[test]
    fn loads_all_typed_sections_and_format_overrides() {
        let path = write_config(
            "complete",
            r##"
[runtime]
refresh_seconds = 2
process_refresh_seconds = 7

[scanner]
git = false
output_tail = false

[tui]
enabled = false
bell = false

[palette]
colors = "#111111,#222222,#000000,#ff0000,#00ff00,#ffff00,#0000ff,#ff00ff,#00ffff,#cccccc,#555555,#ff5555,#55ff55,#ffff55,#5555ff,#ff55ff,#55ffff,#ffffff"

[socket]
path = "/tmp/ilmari-test.sock"

[mcp]
port = 0

[view]
app = true
git = false
detail = true
time = false
output = false
stats = true
remember = false

[badges]
enabled = false
separator = "|"
[badges.waiting_input]
symbol = "WAIT"
style = "fg=red,bold"

[status]
enabled = false
[status.running]
symbol = "run"
style = "fg=cyan"
"##,
        );

        let loaded = LoadedConfig::load_from_path(&path).expect("valid config");
        let config = &loaded.values;
        assert!(loaded.loaded_from_file);
        assert_eq!(config.runtime.refresh_interval(), Duration::from_secs(2));
        assert_eq!(config.runtime.process_refresh_interval(), Duration::from_secs(7));
        assert!(!config.scanner.git);
        assert!(!config.scanner.output_tail);
        assert!(!config.tui.enabled);
        assert!(!config.tui.bell);
        assert!(config.socket.enabled, "a custom path implicitly enables the socket");
        assert_eq!(config.socket.path.as_deref(), Some(Path::new("/tmp/ilmari-test.sock")));
        assert!(config.mcp.enabled, "a configured port implicitly enables MCP");
        assert_eq!(config.mcp.port, 0);
        assert_eq!(
            config.view,
            ViewConfig {
                app: true,
                git: false,
                detail: true,
                time: false,
                output: false,
                stats: true,
                remember: false,
            }
        );
        assert!(!config.badges.enabled);
        assert_eq!(config.badges.separator, "|");
        assert_eq!(config.badges.waiting_input, state_format("WAIT", "fg=red,bold"));
        assert!(!config.status.enabled);
        assert_eq!(config.status.running, state_format("run", "fg=cyan"));
    }

    #[test]
    fn malformed_unknown_and_invalid_values_are_clear_errors() {
        let malformed = write_config("malformed", "[view\napp = true");
        let error = LoadedConfig::load_from_path(&malformed).unwrap_err().to_string();
        assert!(error.contains("invalid Ilmari config"));
        assert!(error.contains(malformed.to_string_lossy().as_ref()));

        let unknown = write_config("unknown", "[view]\nshow_magic = true\n");
        let error = LoadedConfig::load_from_path(&unknown).unwrap_err().to_string();
        assert!(error.contains("unknown field"));
        assert!(error.contains("show_magic"));

        let zero = write_config("zero", "[runtime]\nrefresh_seconds = 0\n");
        let error = LoadedConfig::load_from_path(&zero).unwrap_err().to_string();
        assert!(error.contains("runtime.refresh_seconds"));
        assert!(error.contains("positive"));

        let palette = write_config("palette", "[palette]\ncolors = \"red,blue\"\n");
        let error = LoadedConfig::load_from_path(&palette).unwrap_err().to_string();
        assert!(error.contains("palette.colors"));
        assert!(error.contains("18 comma-separated"));
    }

    #[test]
    fn view_precedence_and_pinning_are_field_aware() {
        let path = write_config("precedence", "[view]\napp = true\ntime = false\n");
        let loaded = LoadedConfig::load_from_path(path).expect("valid config");
        let remembered = ViewState {
            app: false,
            git: false,
            detail: true,
            time: true,
            output: false,
            stats: true,
        };
        let resolved = loaded.resolve_views(
            ViewOverrides { app: Some(false), git: Some(true), ..ViewOverrides::default() },
            Some(&remembered),
        );

        assert_eq!(
            resolved.values,
            ViewState {
                app: false,
                git: true,
                detail: true,
                time: false,
                output: false,
                stats: true,
            }
        );
        assert_eq!(
            resolved.pinned,
            ViewPins { app: true, git: true, detail: true, time: true, output: true, stats: true }
        );
    }

    #[test]
    fn disabling_remember_ignores_state_and_leaves_built_ins_responsive() {
        let path = write_config("no-remember", "[view]\nremember = false\napp = true\n");
        let loaded = LoadedConfig::load_from_path(path).expect("valid config");
        let remembered = ViewState {
            app: false,
            git: false,
            detail: true,
            time: false,
            output: false,
            stats: true,
        };
        let resolved = loaded.resolve_views(ViewOverrides::default(), Some(&remembered));

        assert!(resolved.values.app);
        assert!(resolved.values.git);
        assert!(!resolved.values.detail);
        assert!(resolved.values.time);
        assert!(resolved.values.output);
        assert!(!resolved.values.stats);
        assert!(resolved.pinned.app);
        assert!(!resolved.pinned.detail);
        assert!(!resolved.remember);
    }
}
