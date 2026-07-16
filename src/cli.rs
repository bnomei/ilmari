//! CLI argument parsing and config overlay for Ilmari.
//!
//! `--help` and `--version` short-circuit before environment-backed defaults load;
//! run flags override the same settings resolved by `AppConfig::from_env`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};

use crate::app::AppConfig;
use crate::colors::Palette;
use crate::config::ViewOverrides;

/// Parsed top-level command after argv handling.
pub enum CliCommand {
    Run(AppConfig),
    DaemonStart(AppConfig),
    DaemonStop(AppConfig),
    DaemonStatus(AppConfig),
    Status(AppConfig),
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliOptions {
    refresh_interval: Option<Duration>,
    process_refresh_interval: Option<Duration>,
    palette: Option<Palette>,
    tui_enabled: Option<bool>,
    show_git: Option<bool>,
    bell_enabled: Option<bool>,
    output_tail_capture_enabled: Option<bool>,
    socket_enabled: Option<bool>,
    socket_path: Option<PathBuf>,
    mcp_enabled: Option<bool>,
    mcp_port: Option<u16>,
    view_overrides: ViewOverrides,
}

impl CliOptions {
    fn apply_to(self, mut config: AppConfig) -> AppConfig {
        if let Some(refresh_interval) = self.refresh_interval {
            config.refresh_interval = refresh_interval;
        }
        if let Some(process_refresh_interval) = self.process_refresh_interval {
            config.process_refresh_interval = process_refresh_interval;
        }
        if let Some(palette) = self.palette {
            config.palette = palette;
        }
        if let Some(tui_enabled) = self.tui_enabled {
            config.tui_enabled = tui_enabled;
        }
        if let Some(show_git) = self.show_git {
            config.show_git = show_git;
            config.views.values.git = show_git;
        }
        if let Some(bell_enabled) = self.bell_enabled {
            config.bell_enabled = bell_enabled;
        }
        if let Some(output_tail_capture_enabled) = self.output_tail_capture_enabled {
            config.output_tail_capture_enabled = output_tail_capture_enabled;
        }
        if let Some(socket_path) = self.socket_path {
            config.ipc.socket_path = socket_path;
            config.ipc.enabled = true;
        }
        if let Some(socket_enabled) = self.socket_enabled {
            config.ipc.enabled = socket_enabled;
        }
        if let Some(mcp_port) = self.mcp_port {
            config.mcp.port = mcp_port;
            config.mcp.enabled = true;
        }
        if let Some(mcp_enabled) = self.mcp_enabled {
            config.mcp.enabled = mcp_enabled;
        }
        config
    }
}

/// Parse argv into a command, loading `AppConfig` from the environment for `Run`.
pub fn parse_args<I, S>(args: I) -> Result<CliCommand>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<String>>();
    if args.first().is_some_and(|arg| arg == "daemon") {
        let Some(action) = args.get(1).map(String::as_str) else {
            bail!("daemon requires start, stop, or status; try `ilmari --help`");
        };
        let trailing = args.iter().skip(2).cloned().collect::<Vec<_>>();
        return match action {
            "start" => parse_run_args(trailing, AppConfig::load, CliCommand::DaemonStart),
            "stop" if trailing.is_empty() => {
                Ok(CliCommand::DaemonStop(AppConfig::load(ViewOverrides::default())?))
            }
            "status" if trailing.is_empty() => {
                Ok(CliCommand::DaemonStatus(AppConfig::load(ViewOverrides::default())?))
            }
            "stop" | "status" => bail!("daemon {action} does not accept options"),
            _ => bail!("unknown daemon action `{action}`; expected start, stop, or status"),
        };
    }
    if args.first().is_some_and(|arg| arg == "status") {
        if args.len() != 1 {
            bail!("status does not accept options");
        }
        return Ok(CliCommand::Status(AppConfig::load(ViewOverrides::default())?));
    }
    parse_args_with_config_loader(args, AppConfig::load)
}

#[cfg(test)]
fn parse_args_with_config<I, S>(args: I, base_config: AppConfig) -> Result<CliCommand>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    parse_args_with_config_loader(args, |_| Ok(base_config))
}

fn parse_args_with_config_loader<I, S, F>(args: I, base_config: F) -> Result<CliCommand>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    F: FnOnce(ViewOverrides) -> Result<AppConfig>,
{
    parse_run_args(args, base_config, CliCommand::Run)
}

fn parse_run_args<I, S, F>(
    args: I,
    base_config: F,
    wrap: fn(AppConfig) -> CliCommand,
) -> Result<CliCommand>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    F: FnOnce(ViewOverrides) -> Result<AppConfig>,
{
    let mut args = args.into_iter().map(Into::into).peekable();
    let mut options = CliOptions {
        refresh_interval: None,
        process_refresh_interval: None,
        palette: None,
        tui_enabled: None,
        show_git: None,
        bell_enabled: None,
        output_tail_capture_enabled: None,
        socket_enabled: None,
        socket_path: None,
        mcp_enabled: None,
        mcp_port: None,
        view_overrides: ViewOverrides::default(),
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "-V" | "--version" => return Ok(CliCommand::Version),
            "--refresh-seconds" => {
                let value = next_value(&mut args, "--refresh-seconds")?;
                options.refresh_interval =
                    Some(parse_positive_seconds(&value, "--refresh-seconds")?);
            }
            "--process-refresh-seconds" => {
                let value = next_value(&mut args, "--process-refresh-seconds")?;
                options.process_refresh_interval =
                    Some(parse_positive_seconds(&value, "--process-refresh-seconds")?);
            }
            "--palette" => {
                let value = next_value(&mut args, "--palette")?;
                options.palette = Some(parse_palette(&value)?);
            }
            "--no-tui" => options.tui_enabled = Some(false),
            "--no-git" => {
                options.show_git = Some(false);
                options.view_overrides.git = Some(false);
            }
            "--no-bell" => options.bell_enabled = Some(false),
            "--no-output-tail" => options.output_tail_capture_enabled = Some(false),
            "--socket" => options.socket_enabled = Some(true),
            "--no-socket" => options.socket_enabled = Some(false),
            "--socket-path" => {
                let value = next_value(&mut args, "--socket-path")?;
                options.socket_path = Some(PathBuf::from(value));
            }
            "--mcp" => options.mcp_enabled = Some(true),
            "--no-mcp" => options.mcp_enabled = Some(false),
            "--mcp-port" => {
                let value = next_value(&mut args, "--mcp-port")?;
                options.mcp_port = Some(parse_port(&value, "--mcp-port")?);
            }
            _ => {
                if let Some(value) = arg.strip_prefix("--refresh-seconds=") {
                    options.refresh_interval =
                        Some(parse_positive_seconds(value, "--refresh-seconds")?);
                } else if let Some(value) = arg.strip_prefix("--process-refresh-seconds=") {
                    options.process_refresh_interval =
                        Some(parse_positive_seconds(value, "--process-refresh-seconds")?);
                } else if let Some(value) = arg.strip_prefix("--palette=") {
                    options.palette = Some(parse_palette(value)?);
                } else if let Some(value) = arg.strip_prefix("--socket-path=") {
                    options.socket_path = Some(PathBuf::from(value));
                } else if let Some(value) = arg.strip_prefix("--mcp-port=") {
                    options.mcp_port = Some(parse_port(value, "--mcp-port")?);
                } else {
                    bail!("unknown argument `{arg}`; try `ilmari --help`");
                }
            }
        }
    }

    let view_overrides = options.view_overrides;
    let config = base_config(view_overrides)?;
    Ok(wrap(options.apply_to(config)))
}

/// Static help text including version, flags, and environment variable names.
pub fn help_text() -> &'static str {
    concat!(
        "ilmari ",
        env!("CARGO_PKG_VERSION"),
        "\n\n",
        "Usage: ilmari [OPTIONS]\n",
        "       ilmari daemon <start|stop|status>\n",
        "       ilmari status\n\n",
        "Options:\n",
        "  --refresh-seconds <SECONDS>          Main tmux scan cadence\n",
        "  --process-refresh-seconds <SECONDS>  CPU and memory sampling cadence\n",
        "  --palette <CSV>                      18-slot terminal palette override\n",
        "  --no-tui                             Run headless without terminal UI\n",
        "  --no-git                             Start with git summaries hidden\n",
        "  --no-output-tail                     Disable tmux capture-pane output tails\n",
        "  --no-bell                            Disable terminal bell alerts\n",
        "  --socket                             Enable local JSON socket publishing\n",
        "  --no-socket                          Disable local JSON socket publishing\n",
        "  --socket-path <PATH>                  Local JSON socket path override\n",
        "  --mcp                                Enable loopback MCP resource server\n",
        "  --no-mcp                             Disable loopback MCP resource server\n",
        "  --mcp-port <PORT>                     Loopback MCP port, default 62778; 0 chooses a free port\n",
        "  -h, --help                           Print help\n",
        "  -V, --version                        Print version\n\n",
        "Configuration: $XDG_CONFIG_HOME/ilmari/config.toml or ~/.config/ilmari/config.toml\n",
        "Flags override TOML settings for the same run.\n",
    )
}

/// One-line version string for `--version`.
pub fn version_text() -> &'static str {
    concat!("ilmari ", env!("CARGO_PKG_VERSION"))
}

fn next_value<I>(args: &mut std::iter::Peekable<I>, flag: &str) -> Result<String>
where
    I: Iterator<Item = String>,
{
    let Some(value) = args.next() else {
        bail!("{flag} requires a value");
    };
    if value.starts_with('-') {
        bail!("{flag} requires a value");
    }
    Ok(value)
}

fn parse_positive_seconds(value: &str, flag: &str) -> Result<Duration> {
    let parsed =
        value.trim().parse::<u64>().ok().filter(|seconds| *seconds > 0).map(Duration::from_secs);
    let Some(parsed) = parsed else {
        bail!("{flag} requires a positive integer number of seconds");
    };
    Ok(parsed)
}

fn parse_palette(value: &str) -> Result<Palette> {
    Palette::from_csv(value).map_err(|error| anyhow::anyhow!("invalid --palette value: {error}"))
}

fn parse_port(value: &str, flag: &str) -> Result<u16> {
    value
        .trim()
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("{flag} requires a port from 0 to 65535"))
}

#[cfg(test)]
mod tests {
    use super::{
        help_text, parse_args, parse_args_with_config, parse_args_with_config_loader, version_text,
        CliCommand,
    };
    use crate::app::AppConfig;
    use crate::colors::Palette;
    use std::collections::BTreeMap;
    use std::time::Duration;

    #[test]
    fn help_and_version_short_circuit_before_config() {
        assert!(matches!(parse_args(["--help"]).expect("help parses"), CliCommand::Help));
        assert!(matches!(parse_args(["--version"]).expect("version parses"), CliCommand::Version));
        for flag in [
            "--refresh-seconds",
            "--process-refresh-seconds",
            "--palette",
            "--no-tui",
            "--no-git",
            "--no-output-tail",
            "--no-bell",
            "--socket",
            "--no-socket",
            "--socket-path",
            "--mcp",
            "--no-mcp",
            "--mcp-port",
            "--help",
            "--version",
        ] {
            assert!(help_text().contains(flag), "help should mention {flag}");
        }
        assert!(help_text().contains("Flags override TOML settings"));
        assert_eq!(version_text(), concat!("ilmari ", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn help_and_version_do_not_load_runtime_config() {
        assert!(matches!(
            parse_args_with_config_loader(["--help"], |_| panic!("config should not load"))
                .expect("help parses"),
            CliCommand::Help
        ));
        assert!(matches!(
            parse_args_with_config_loader(["--version"], |_| panic!("config should not load"))
                .expect("version parses"),
            CliCommand::Version
        ));
    }

    #[test]
    fn flags_override_runtime_defaults() {
        let command = parse_args([
            "--refresh-seconds",
            "7",
            "--process-refresh-seconds=31",
            "--no-tui",
            "--no-git",
            "--no-output-tail",
            "--no-bell",
            "--no-socket",
            "--mcp-port=0",
        ])
        .expect("flags should parse");

        let CliCommand::Run(config) = command else {
            panic!("expected run command");
        };

        assert_eq!(config.refresh_interval, Duration::from_secs(7));
        assert_eq!(config.process_refresh_interval, Duration::from_secs(31));
        assert!(!config.tui_enabled);
        assert!(!config.show_git);
        assert!(!config.output_tail_capture_enabled);
        assert!(!config.bell_enabled);
        assert!(!config.ipc.enabled);
        assert!(config.mcp.enabled);
        assert_eq!(config.mcp.port, 0);
    }

    #[test]
    fn invalid_flag_values_are_errors() {
        assert!(parse_args(["--refresh-seconds", "0"]).is_err());
        assert!(parse_args(["--process-refresh-seconds=abc"]).is_err());
        assert!(parse_args(["--mcp-port", "99999"]).is_err());
        assert!(parse_args(["--unknown"]).is_err());
    }

    #[test]
    fn daemon_and_compact_status_commands_parse_without_changing_normal_run() {
        assert!(matches!(
            parse_args(["daemon", "start", "--no-tui"]).expect("daemon start"),
            CliCommand::DaemonStart(_)
        ));
        assert!(matches!(
            parse_args(["daemon", "stop"]).expect("daemon stop"),
            CliCommand::DaemonStop(_)
        ));
        assert!(matches!(
            parse_args(["daemon", "status"]).expect("daemon status"),
            CliCommand::DaemonStatus(_)
        ));
        assert!(matches!(parse_args(["status"]).expect("status"), CliCommand::Status(_)));
        assert!(parse_args(["daemon", "unknown"]).is_err());
    }

    #[test]
    fn flags_override_config_and_legacy_ilmari_environment_is_ignored() {
        let flag_palette =
            "#111111,#222222,#000000,#ff0000,#00ff00,#ffff00,#0000ff,#ff00ff,#00ffff,#cccccc,#555555,#ff5555,#55ff55,#ffff55,#5555ff,#ff55ff,#55ffff,#ffffff";
        let mut env = BTreeMap::new();
        env.insert("ILMARI_REFRESH_SECONDS".to_string(), "123".to_string());
        env.insert("ILMARI_PROCESS_REFRESH_SECONDS".to_string(), "456".to_string());
        env.insert("ILMARI_TUI".to_string(), "0".to_string());
        env.insert("ILMARI_OUTPUT_TAIL".to_string(), "true".to_string());
        env.insert("ILMARI_SOCKET".to_string(), "0".to_string());
        env.insert("ILMARI_SOCKET_PATH".to_string(), "/tmp/env.sock".to_string());
        env.insert("ILMARI_MCP".to_string(), "0".to_string());
        env.insert("ILMARI_MCP_PORT".to_string(), "8888".to_string());
        env.insert("ILMARI_TUI_PALETTE".to_string(), "ignored".to_string());

        let command = parse_args_with_config(
            [
                "--refresh-seconds",
                "9",
                "--process-refresh-seconds",
                "44",
                "--no-tui",
                "--palette",
                flag_palette,
                "--no-git",
                "--no-output-tail",
                "--no-bell",
                "--socket-path=/tmp/flag.sock",
                "--mcp",
                "--mcp-port",
                "9999",
            ],
            AppConfig::from_env_map(&env, crate::config::ViewOverrides::default())
                .expect("built-in config should load"),
        )
        .expect("flags should parse");

        let CliCommand::Run(config) = command else {
            panic!("expected run command");
        };

        assert_eq!(config.refresh_interval, Duration::from_secs(9));
        assert_eq!(config.process_refresh_interval, Duration::from_secs(44));
        assert_eq!(config.palette, Palette::from_csv(flag_palette).expect("flag palette parses"));
        assert!(!config.tui_enabled);
        assert!(!config.show_git);
        assert!(!config.output_tail_capture_enabled);
        assert!(!config.bell_enabled);
        assert!(config.ipc.enabled);
        assert_eq!(config.ipc.socket_path, std::path::PathBuf::from("/tmp/flag.sock"));
        assert!(config.mcp.enabled);
        assert_eq!(config.mcp.port, 9999);
    }
}
