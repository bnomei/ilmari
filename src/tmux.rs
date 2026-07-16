//! Tmux subprocess boundary for pane snapshots, output capture, and focus commands.
//!
//! Ilmari treats tmux as the source of truth for which agent panes exist and what
//! they last printed. Commands here wrap `list-panes` and `capture-pane`, parse the
//! tab-separated format into stable pane ids, and surface per-line parse failures as
//! warnings so one malformed row cannot blank the whole radar.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agents::SessionTracker;
use crate::model::AgentKind;

/// Tab-separated `list-panes -aF` format consumed by `PaneSnapshot::parse`.
pub const LIST_PANES_FORMAT: &str = "#{pane_id}\t#{pane_pid}\t#{session_id}\t#{session_name}\t#{window_id}\t#{window_name}\t#{pane_dead}\t#{pane_current_path}\t#{pane_current_command}\t#{pane_title}";
/// Default `capture-pane -S` window for output-tail classification.
pub const DEFAULT_CAPTURE_START: &str = "-80";
const PANE_SNAPSHOT_FIELD_COUNT: usize = 10;
static ORIGIN_SERVER_IDENTITY: OnceLock<Option<TmuxServerIdentity>> = OnceLock::new();

/// Immutable identity of the tmux server generation that originated this process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxServerIdentity {
    pub socket_path: PathBuf,
    pub server_pid: u32,
    pub socket_device: Option<u64>,
    pub socket_inode: Option<u64>,
}

/// Parsed tmux pane row from `list-panes -aF` with stable session, window, and pane ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub pane_id: String,
    pub pane_pid: Option<u32>,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_dead: bool,
    pub pane_current_path: PathBuf,
    pub pane_current_command: String,
    pub pane_title: String,
}

impl PaneSnapshot {
    pub fn parse(line: &str) -> Result<Self, PaneSnapshotParseError> {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != PANE_SNAPSHOT_FIELD_COUNT {
            return Err(PaneSnapshotParseError::InvalidFieldCount {
                expected: PANE_SNAPSHOT_FIELD_COUNT,
                actual: fields.len(),
            });
        }

        Ok(Self {
            pane_id: parse_required_field(fields[0], "pane_id")?.to_string(),
            pane_pid: parse_optional_u32(fields[1], "pane_pid")?,
            session_id: parse_required_field(fields[2], "session_id")?.to_string(),
            session_name: parse_required_field(fields[3], "session_name")?.to_string(),
            window_id: parse_required_field(fields[4], "window_id")?.to_string(),
            window_name: parse_required_field(fields[5], "window_name")?.to_string(),
            pane_dead: parse_bool_flag(fields[6], "pane_dead")?,
            pane_current_path: PathBuf::from(fields[7]),
            pane_current_command: fields[8].to_string(),
            pane_title: fields[9].to_string(),
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PaneSnapshotParseError {
    #[error("expected {expected} tab-separated fields, got {actual}")]
    InvalidFieldCount { expected: usize, actual: usize },
    #[error("missing required field `{field}`")]
    MissingField { field: &'static str },
    #[error("invalid unsigned integer for `{field}`: `{value}`")]
    InvalidUnsignedInteger { field: &'static str, value: String },
    #[error("invalid boolean flag for `{field}`: `{value}`")]
    InvalidBooleanFlag { field: &'static str, value: String },
}

fn parse_required_field<'a>(
    value: &'a str,
    field: &'static str,
) -> Result<&'a str, PaneSnapshotParseError> {
    if value.is_empty() {
        return Err(PaneSnapshotParseError::MissingField { field });
    }

    Ok(value)
}

fn parse_optional_u32(
    value: &str,
    field: &'static str,
) -> Result<Option<u32>, PaneSnapshotParseError> {
    if value.is_empty() {
        return Ok(None);
    }

    value.parse::<u32>().map(Some).map_err(|_| PaneSnapshotParseError::InvalidUnsignedInteger {
        field,
        value: value.to_string(),
    })
}

fn parse_bool_flag(value: &str, field: &'static str) -> Result<bool, PaneSnapshotParseError> {
    match value {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err(PaneSnapshotParseError::InvalidBooleanFlag { field, value: value.to_string() }),
    }
}

/// Renderable `tmux` argv list executed as one subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxCommand {
    args: Vec<String>,
}

impl TmuxCommand {
    pub fn new(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { args: args.into_iter().map(Into::into).collect() }
    }

    #[cfg(test)]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    #[cfg(test)]
    pub fn argv_for_socket(&self, socket_path: &Path) -> Vec<String> {
        let mut argv = vec!["-S".to_string(), socket_path.display().to_string()];
        argv.extend(self.args.iter().cloned());
        argv
    }

    fn as_command(&self, socket_path: &Path) -> Command {
        let mut command = Command::new("tmux");
        command.arg("-S").arg(socket_path).args(&self.args);
        command
    }

    fn render(&self, socket_path: &Path) -> String {
        format!("tmux -S {} {}", socket_path.display(), self.args.join(" "))
    }
}

/// Failure from executing or decoding a tmux subprocess.
#[derive(Debug, Error)]
pub enum TmuxError {
    #[error("tmux socket is unavailable; run Ilmari from the target tmux server")]
    MissingSocket,
    #[error("failed to execute tmux: {0}")]
    Io(#[from] io::Error),
    #[error("tmux output was not valid utf-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("tmux command failed: {command} (exit code: {exit_code:?}) {stderr}")]
    CommandFailed { command: String, exit_code: Option<i32>, stderr: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTailCaptureFailure {
    pub pane_id: String,
    pub error: String,
}

/// Per-pane `capture-pane` results collected only for panes that need fresh output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputTailCapture {
    pub output_tails: HashMap<String, String>,
    pub failures: Vec<OutputTailCaptureFailure>,
}

/// Build the global `list-panes -aF` command used for radar snapshots.
pub fn pane_snapshot_command() -> TmuxCommand {
    TmuxCommand::new(["list-panes", "-aF", LIST_PANES_FORMAT])
}

/// Batch of parsed pane snapshots plus non-fatal warnings for malformed `list-panes` lines.
///
/// A tab embedded in a free-form field or an invalid flag drops only that pane; healthy
/// rows still populate `snapshots` while the bad line is recorded in `warnings`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneSnapshotCollection {
    pub snapshots: Vec<PaneSnapshot>,
    pub warnings: Vec<String>,
}

/// Run `list-panes` and parse all visible pane rows, retaining per-line warnings.
pub fn collect_pane_snapshots() -> Result<PaneSnapshotCollection, TmuxError> {
    let stdout = run_tmux_command(&pane_snapshot_command())?;
    Ok(parse_pane_snapshots(&stdout))
}

pub fn parse_pane_snapshots(stdout: &str) -> PaneSnapshotCollection {
    let mut collection = PaneSnapshotCollection::default();
    for (index, line) in stdout.lines().filter(|line| !line.trim().is_empty()).enumerate() {
        match PaneSnapshot::parse(line) {
            Ok(snapshot) => collection.snapshots.push(snapshot),
            Err(source) => collection
                .warnings
                .push(format!("tmux: skipped malformed pane line {}: {source}", index + 1)),
        }
    }
    collection
}

pub fn capture_output_tail_command(target: &str, start: &str) -> TmuxCommand {
    TmuxCommand::new(["capture-pane", "-p", "-J", "-t", target, "-S", start])
}

pub fn capture_output_tail(target: &str, start: &str) -> Result<String, TmuxError> {
    run_tmux_command(&capture_output_tail_command(target, start))
}

/// Capture output tails only for panes whose adapters require fresh terminal text.
pub fn capture_output_tails_with_process_kinds(
    panes: &[PaneSnapshot],
    tracker: &SessionTracker,
    process_kinds: &HashMap<String, AgentKind>,
) -> OutputTailCapture {
    capture_output_tails_with_process_kinds_using(
        panes,
        tracker,
        process_kinds,
        capture_output_tail,
    )
}

fn capture_output_tails_with_process_kinds_using(
    panes: &[PaneSnapshot],
    tracker: &SessionTracker,
    process_kinds: &HashMap<String, AgentKind>,
    mut capture_tail: impl FnMut(&str, &str) -> Result<String, TmuxError>,
) -> OutputTailCapture {
    let previous = tracker.records();
    let mut capture = OutputTailCapture::default();

    for pane in panes {
        let previous = previous.get(&pane.pane_id);
        if !tracker.registry().needs_output_tail(
            pane,
            previous,
            process_kinds.get(&pane.pane_id).copied(),
        ) {
            continue;
        }

        match capture_tail(&pane.pane_id, DEFAULT_CAPTURE_START) {
            Ok(output_tail) => {
                capture.output_tails.insert(pane.pane_id.clone(), output_tail);
            }
            Err(error) => capture.failures.push(OutputTailCaptureFailure {
                pane_id: pane.pane_id.clone(),
                error: error.to_string(),
            }),
        }
    }

    capture
}

#[cfg_attr(not(feature = "tui"), allow(dead_code))]
pub fn jump_command(target: &PaneSnapshot) -> TmuxCommand {
    TmuxCommand::new([
        "switch-client",
        "-t",
        target.session_id.as_str(),
        ";",
        "select-window",
        "-t",
        target.window_id.as_str(),
        ";",
        "select-pane",
        "-t",
        target.pane_id.as_str(),
    ])
}

#[cfg_attr(not(feature = "tui"), allow(dead_code))]
pub fn jump_to_pane(target: &PaneSnapshot) -> Result<(), TmuxError> {
    run_tmux_command(&jump_command(target))?;
    Ok(())
}

/// Set a quiet global tmux option, used to publish socket and MCP discovery paths.
pub fn set_global_option(name: &str, value: &str) -> Result<(), TmuxError> {
    run_tmux_command(&TmuxCommand::new(["set-option", "-gq", name, value]))?;
    Ok(())
}

/// Parse the tmux socket path by removing the numeric server/client suffix from the right.
///
/// Socket paths may contain commas, so splitting at the first comma is incorrect.
pub(crate) fn socket_path_from_tmux_context(value: &str) -> Option<PathBuf> {
    tmux_context_from_value(value).map(|(path, _server_pid)| path)
}

fn tmux_context_from_value(value: &str) -> Option<(PathBuf, u32)> {
    let (path_and_pid, client) = value.rsplit_once(',')?;
    client.parse::<u64>().ok()?;
    let (path, pid) = path_and_pid.rsplit_once(',')?;
    let server_pid = pid.parse::<u32>().ok()?;
    (!path.is_empty()).then(|| (PathBuf::from(path), server_pid))
}

fn bind_origin_identity(value: &str, probed: TmuxServerIdentity) -> Option<TmuxServerIdentity> {
    let (context_path, context_server_pid) = tmux_context_from_value(value)?;
    let canonical_context_path = fs::canonicalize(&context_path).unwrap_or(context_path);
    (probed.server_pid == context_server_pid && probed.socket_path == canonical_context_path)
        .then_some(probed)
}

/// Resolve and freeze the tmux server generation that originated this process.
pub fn origin_server_identity() -> Option<&'static TmuxServerIdentity> {
    ORIGIN_SERVER_IDENTITY
        .get_or_init(|| {
            let context = env::var_os("TMUX")?;
            let context = context.to_string_lossy();
            let (candidate, _context_server_pid) = tmux_context_from_value(context.as_ref())?;
            let probed = probe_server_identity_at(&candidate)?;
            bind_origin_identity(context.as_ref(), probed)
        })
        .as_ref()
}

/// Return the canonical socket path for the frozen originating tmux generation.
pub fn origin_socket_path() -> Option<&'static Path> {
    origin_server_identity().map(|identity| identity.socket_path.as_path())
}

/// Return whether the exact originating tmux server generation still answers.
pub fn server_is_alive() -> bool {
    origin_server_identity().is_some_and(|origin| {
        probe_server_identity_at(&origin.socket_path).as_ref() == Some(origin)
    })
}

fn probe_server_identity_at(socket_path: &Path) -> Option<TmuxServerIdentity> {
    let output = Command::new("tmux")
        .arg("-S")
        .arg(socket_path)
        .args(["display-message", "-p", "#{socket_path}\t#{pid}"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    let (reported_path, server_pid) = stdout.trim_end().rsplit_once('\t')?;
    let server_pid = server_pid.parse::<u32>().ok()?;
    let reported_path = PathBuf::from(reported_path);
    let canonical_path = fs::canonicalize(&reported_path).unwrap_or(reported_path);
    let (socket_device, socket_inode) = socket_file_identity(&canonical_path);
    Some(TmuxServerIdentity {
        socket_path: canonical_path,
        server_pid,
        socket_device,
        socket_inode,
    })
}

#[cfg(unix)]
fn socket_file_identity(path: &Path) -> (Option<u64>, Option<u64>) {
    fs::metadata(path)
        .map(|metadata| (Some(metadata.dev()), Some(metadata.ino())))
        .unwrap_or_default()
}

#[cfg(not(unix))]
fn socket_file_identity(_path: &Path) -> (Option<u64>, Option<u64>) {
    (None, None)
}

/// Read one global user option from the originating server.
pub fn global_option(name: &str) -> Result<String, TmuxError> {
    run_tmux_command(&TmuxCommand::new(["show-option", "-gqv", name]))
        .map(|value| value.trim_end().to_string())
}

/// Remove one global user option from the originating server.
pub fn unset_global_option(name: &str) -> Result<(), TmuxError> {
    run_tmux_command(&TmuxCommand::new(["set-option", "-gu", name]))?;
    Ok(())
}

/// Set or clear a pane-local user option on an exact pane id.
pub fn set_pane_option(pane_id: &str, name: &str, value: Option<&str>) -> Result<(), TmuxError> {
    let command = match value {
        Some(value) => TmuxCommand::new(["set-option", "-pq", "-t", pane_id, name, value]),
        None => TmuxCommand::new(["set-option", "-pu", "-t", pane_id, name]),
    };
    run_tmux_command(&command)?;
    Ok(())
}

/// Collect exact panes currently focused by any attached tmux client.
pub fn focused_pane_ids() -> Result<HashSet<String>, TmuxError> {
    let output = run_tmux_command(&TmuxCommand::new(["list-clients", "-F", "#{pane_id}"]))?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|pane_id| pane_id.starts_with('%'))
        .map(ToOwned::to_owned)
        .collect())
}

/// Focused panes and live renderer overrides sampled in one tmux round trip.
pub struct FocusRendererState {
    pub focused_pane_ids: HashSet<String>,
    pub badges_enabled: Option<String>,
    pub status_enabled: Option<String>,
}

pub fn focus_and_renderer_overrides() -> Result<FocusRendererState, TmuxError> {
    let output = run_tmux_command(&TmuxCommand::new([
        "list-clients",
        "-F",
        "focus:#{pane_id}",
        ";",
        "display-message",
        "-p",
        "badges:#{@ilmari_badges_enabled}",
        ";",
        "display-message",
        "-p",
        "status:#{@ilmari_status_enabled}",
    ]))?;
    let mut state = FocusRendererState {
        focused_pane_ids: HashSet::new(),
        badges_enabled: None,
        status_enabled: None,
    };
    for line in output.lines() {
        if let Some(pane_id) =
            line.strip_prefix("focus:").filter(|pane_id| pane_id.starts_with('%'))
        {
            state.focused_pane_ids.insert(pane_id.to_string());
        } else if let Some(value) = line.strip_prefix("badges:") {
            state.badges_enabled = (!value.is_empty()).then(|| value.to_string());
        } else if let Some(value) = line.strip_prefix("status:") {
            state.status_enabled = (!value.is_empty()).then(|| value.to_string());
        }
    }
    Ok(state)
}

/// Clear daemon-owned publication without removing endpoints published by another role.
pub fn clear_published_state(
    owned_daemon_pid: u32,
    owned_socket_path: Option<&Path>,
    owned_mcp_url: Option<&str>,
) {
    let owned_daemon_pid = owned_daemon_pid.to_string();
    let owned_socket_path = owned_socket_path.map(|path| path.to_string_lossy().into_owned());
    if !daemon_publication_matches(
        global_option("@ilmari_daemon_owner_pid").ok().as_deref(),
        global_option("@ilmari_daemon_socket_path").ok().as_deref(),
        global_option("@ilmari_daemon_mcp_url").ok().as_deref(),
        &owned_daemon_pid,
        owned_socket_path.as_deref(),
        owned_mcp_url,
    ) {
        return;
    }
    if let Ok(collection) = collect_pane_snapshots() {
        for pane in collection.snapshots {
            let _ = set_pane_option(&pane.pane_id, "@ilmari_state", None);
            let _ = set_pane_option(&pane.pane_id, "@ilmari_badge", None);
        }
    }
    for option in [
        "@ilmari_window_badges",
        "@ilmari_status_summary",
        "@ilmari_running_count",
        "@ilmari_waiting_count",
        "@ilmari_finished_count",
    ] {
        let _ = unset_global_option(option);
    }
    if let Some(path) = owned_socket_path.as_deref() {
        unset_global_option_if_value("@ilmari_daemon_socket_path", path);
        unset_global_option_if_value("@ilmari_socket_path", path);
    }
    if let Some(url) = owned_mcp_url {
        unset_global_option_if_value("@ilmari_daemon_mcp_url", url);
        unset_global_option_if_value("@ilmari_mcp_url", url);
    }
    unset_global_option_if_value("@ilmari_daemon_owner_pid", &owned_daemon_pid);
}

fn daemon_publication_matches(
    current_owner_pid: Option<&str>,
    current_socket_path: Option<&str>,
    current_mcp_url: Option<&str>,
    expected_owner_pid: &str,
    expected_socket_path: Option<&str>,
    expected_mcp_url: Option<&str>,
) -> bool {
    current_owner_pid == Some(expected_owner_pid)
        && expected_socket_path.is_none_or(|expected| current_socket_path == Some(expected))
        && expected_mcp_url.is_none_or(|expected| current_mcp_url == Some(expected))
}

fn unset_global_option_if_value(name: &str, expected: &str) {
    if publication_value_matches(global_option(name).ok().as_deref(), expected) {
        let _ = unset_global_option(name);
    }
}

fn publication_value_matches(current: Option<&str>, expected: &str) -> bool {
    current == Some(expected)
}

fn run_tmux_command(command: &TmuxCommand) -> Result<String, TmuxError> {
    let socket_path = origin_socket_path().ok_or(TmuxError::MissingSocket)?;
    if !server_is_alive() {
        return Err(TmuxError::CommandFailed {
            command: command.render(socket_path),
            exit_code: None,
            stderr: "originating tmux server generation changed or disappeared".to_string(),
        });
    }
    let output = command.as_command(socket_path).output()?;
    if !output.status.success() {
        return Err(TmuxError::CommandFailed {
            command: command.render(socket_path),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::{
        bind_origin_identity, capture_output_tail_command,
        capture_output_tails_with_process_kinds_using, daemon_publication_matches, jump_command,
        pane_snapshot_command, parse_pane_snapshots, publication_value_matches,
        socket_path_from_tmux_context, PaneSnapshot, PaneSnapshotParseError, TmuxError,
        TmuxServerIdentity, DEFAULT_CAPTURE_START, LIST_PANES_FORMAT,
    };
    use crate::agents::SessionTracker;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn snapshot_command_uses_global_tab_separated_format() {
        let command = pane_snapshot_command();

        assert_eq!(
            command.args(),
            &["list-panes".to_string(), "-aF".to_string(), LIST_PANES_FORMAT.to_string(),]
        );
    }

    #[test]
    fn every_executed_command_prefixes_the_originating_socket() {
        let command = pane_snapshot_command();
        assert_eq!(
            &command.argv_for_socket(std::path::Path::new("/tmp/tmux-exact"))[..3],
            &["-S", "/tmp/tmux-exact", "list-panes"]
        );
    }

    #[test]
    fn tmux_context_is_parsed_from_the_right_for_comma_socket_paths() {
        assert_eq!(
            socket_path_from_tmux_context("/tmp/ilmari,custom/tmux.sock,4312,7"),
            Some(PathBuf::from("/tmp/ilmari,custom/tmux.sock"))
        );
        assert_eq!(
            socket_path_from_tmux_context("/tmp/plain.sock,4312,0"),
            Some(PathBuf::from("/tmp/plain.sock"))
        );
        assert_eq!(socket_path_from_tmux_context("/tmp/plain.sock,not-a-pid,0"), None);
    }

    #[test]
    fn server_generation_identity_changes_with_pid_or_socket_inode() {
        let first = TmuxServerIdentity {
            socket_path: PathBuf::from("/tmp/tmux.sock"),
            server_pid: 10,
            socket_device: Some(1),
            socket_inode: Some(100),
        };
        let mut replacement = first.clone();
        replacement.server_pid = 11;
        replacement.socket_inode = Some(101);

        assert_ne!(first, replacement);
    }

    #[test]
    fn initial_origin_binding_rejects_replacement_before_first_probe() {
        let responding = TmuxServerIdentity {
            socket_path: PathBuf::from("/tmp/ilmari,custom/tmux.sock"),
            server_pid: 11,
            socket_device: Some(1),
            socket_inode: Some(101),
        };

        assert!(
            bind_origin_identity("/tmp/ilmari,custom/tmux.sock,10,7", responding.clone()).is_none()
        );
        assert_eq!(
            bind_origin_identity("/tmp/ilmari,custom/tmux.sock,11,7", responding.clone()),
            Some(responding.clone())
        );
        assert!(bind_origin_identity("/tmp/another.sock,11,7", responding).is_none());
    }

    #[test]
    fn cleanup_only_removes_exact_daemon_role_publications() {
        assert!(publication_value_matches(
            Some("http://127.0.0.1:4010/mcp"),
            "http://127.0.0.1:4010/mcp"
        ));
        assert!(!publication_value_matches(
            Some("http://127.0.0.1:4020/mcp"),
            "http://127.0.0.1:4010/mcp"
        ));
        assert!(!publication_value_matches(None, "daemon-value"));
        assert!(daemon_publication_matches(
            Some("42"),
            Some("/tmp/daemon.sock"),
            Some("http://127.0.0.1:4010/mcp"),
            "42",
            Some("/tmp/daemon.sock"),
            Some("http://127.0.0.1:4010/mcp"),
        ));
        assert!(!daemon_publication_matches(
            Some("43"),
            Some("/tmp/replacement.sock"),
            Some("http://127.0.0.1:4020/mcp"),
            "42",
            Some("/tmp/daemon.sock"),
            Some("http://127.0.0.1:4010/mcp"),
        ));
    }

    #[test]
    fn parse_pane_snapshots_reads_multiple_rows() {
        let collection = parse_pane_snapshots(
            "%1\t101\t$1\twork\t@1\teditor\t0\t/tmp/api\tcodex\tagent\n%9\t202\t$2\tops\t@3\tlogs\t1\t/tmp/blog\tamp\treview\n",
        );
        let snapshots = collection.snapshots;
        assert!(collection.warnings.is_empty());

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].pane_id, "%1");
        assert_eq!(snapshots[0].pane_pid, Some(101));
        assert_eq!(snapshots[0].session_id, "$1");
        assert_eq!(snapshots[0].pane_current_command, "codex");
        assert_eq!(snapshots[1].pane_id, "%9");
        assert!(snapshots[1].pane_dead);
        assert_eq!(snapshots[1].pane_current_command, "amp");
    }

    #[test]
    fn parse_pane_snapshots_skips_malformed_lines_and_keeps_healthy_ones() {
        let collection = parse_pane_snapshots(
            "%1\t101\t$1\twork\t@1\teditor\t0\t/tmp/api\tcodex\tagent\n%9\t202\t$2\tops\t@3\tlogs\tmaybe\t/tmp/blog\tamp\treview\n",
        );

        assert_eq!(collection.snapshots.len(), 1);
        assert_eq!(collection.snapshots[0].pane_id, "%1");
        assert_eq!(collection.warnings.len(), 1);
        assert!(collection.warnings[0].contains("line 2"));
        assert!(collection.warnings[0].contains("pane_dead"));
    }

    #[test]
    fn parse_pane_snapshots_tolerates_embedded_tab_in_a_field() {
        let collection = parse_pane_snapshots(
            "%1\t101\t$1\twork\t@1\teditor\t0\t/tmp/foo\tbar\tcodex\tmytitle\n%9\t202\t$2\tops\t@3\tlogs\t1\t/tmp/blog\tamp\treview\n",
        );

        assert_eq!(collection.snapshots.len(), 1);
        assert_eq!(collection.snapshots[0].pane_id, "%9");
        assert_eq!(collection.warnings.len(), 1);
        assert!(collection.warnings[0].contains("line 1"));
    }

    #[test]
    fn jump_command_targets_stable_tmux_ids() {
        let target = PaneSnapshot::parse(
            "%12\t301\t$5\tclient\t@8\tagents\t0\t/workspace/ilmari\tcodex\tworker",
        )
        .expect("pane snapshot should parse");

        assert_eq!(
            jump_command(&target).args(),
            &[
                "switch-client".to_string(),
                "-t".to_string(),
                "$5".to_string(),
                ";".to_string(),
                "select-window".to_string(),
                "-t".to_string(),
                "@8".to_string(),
                ";".to_string(),
                "select-pane".to_string(),
                "-t".to_string(),
                "%12".to_string(),
            ]
        );
    }

    #[test]
    fn capture_output_tail_command_joins_wrapped_lines_from_default_tail_window() {
        assert_eq!(
            capture_output_tail_command("%12", DEFAULT_CAPTURE_START).args(),
            &[
                "capture-pane".to_string(),
                "-p".to_string(),
                "-J".to_string(),
                "-t".to_string(),
                "%12".to_string(),
                "-S".to_string(),
                DEFAULT_CAPTURE_START.to_string(),
            ]
        );
    }

    #[test]
    fn capture_output_tails_preserves_per_pane_failures() {
        let tracker = SessionTracker::new();
        let panes = parse_pane_snapshots(
            "%1\t101\t$1\twork\t@1\teditor\t0\t/tmp/api\tcodex\tagent\n%2\t202\t$2\tops\t@3\tagents\t0\t/tmp/blog\tamp\treview\n",
        )
        .snapshots;

        let capture = capture_output_tails_with_process_kinds_using(
            &panes,
            &tracker,
            &HashMap::new(),
            |target, _| {
                if target == "%2" {
                    return Err(TmuxError::CommandFailed {
                        command: "tmux capture-pane".to_string(),
                        exit_code: Some(1),
                        stderr: "pane not found".to_string(),
                    });
                }

                Ok(format!("tail for {target}"))
            },
        );

        assert_eq!(capture.output_tails.get("%1").map(String::as_str), Some("tail for %1"));
        assert!(!capture.output_tails.contains_key("%2"));
        assert_eq!(capture.failures.len(), 1);
        assert_eq!(capture.failures[0].pane_id, "%2");
        assert!(capture.failures[0].error.contains("pane not found"));
    }

    #[test]
    fn pane_snapshot_parser_accepts_blank_optional_fields() {
        let snapshot = PaneSnapshot::parse("%12\t\t$1\tdev\t@3\teditor\t0\t/workspace/ilmari\t\t")
            .expect("snapshot should parse");

        assert_eq!(snapshot.pane_id, "%12");
        assert_eq!(snapshot.pane_pid, None);
        assert_eq!(snapshot.session_id, "$1");
        assert_eq!(snapshot.window_id, "@3");
        assert!(!snapshot.pane_dead);
        assert_eq!(snapshot.pane_current_path, PathBuf::from("/workspace/ilmari"));
        assert_eq!(snapshot.pane_current_command, "");
        assert_eq!(snapshot.pane_title, "");
    }

    #[test]
    fn pane_snapshot_parser_rejects_bad_dead_flag() {
        let error = PaneSnapshot::parse(
            "%12\t123\t$1\tdev\t@3\teditor\tnope\t/workspace/ilmari\tcodex\ttitle",
        )
        .expect_err("invalid pane_dead flag should fail");

        assert_eq!(
            error,
            PaneSnapshotParseError::InvalidBooleanFlag {
                field: "pane_dead",
                value: "nope".to_string(),
            }
        );
    }
}
