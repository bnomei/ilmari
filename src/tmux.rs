//! Tmux subprocess boundary for pane snapshots, output capture, and focus commands.
//!
//! Ilmari treats tmux as the source of truth for which agent panes exist and what
//! they last printed. Commands here wrap `list-panes` and `capture-pane`, parse the
//! tab-separated format into stable pane ids, and surface per-line parse failures as
//! warnings so one malformed row cannot blank the whole radar.

use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
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
const GENERATION_ACCEPTED_MARKER: &str = "__ILMARI_TMUX_GENERATION_ACCEPTED__";
const GENERATION_REJECTED_MARKER: &str = "__ILMARI_TMUX_GENERATION_REJECTED__";
const TPM_ORIGIN_SOCKET_DEVICE_ENV: &str = "ILMARI_TMUX_ORIGIN_SOCKET_DEVICE";
const TPM_ORIGIN_SOCKET_INODE_ENV: &str = "ILMARI_TMUX_ORIGIN_SOCKET_INODE";
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
    parts: Vec<TmuxCommandPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TmuxCommandPart {
    Argument(String),
    Separator,
}

impl TmuxCommand {
    pub fn new(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { parts: args.into_iter().map(|arg| TmuxCommandPart::Argument(arg.into())).collect() }
    }

    fn sequence(commands: impl IntoIterator<Item = Self>) -> Self {
        let mut parts = Vec::new();
        for command in commands {
            if !parts.is_empty() {
                parts.push(TmuxCommandPart::Separator);
            }
            parts.extend(command.parts);
        }
        Self { parts }
    }

    #[cfg(test)]
    pub fn args(&self) -> Vec<String> {
        self.parts
            .iter()
            .map(|part| match part {
                TmuxCommandPart::Argument(arg) => arg.clone(),
                TmuxCommandPart::Separator => ";".to_string(),
            })
            .collect()
    }

    #[cfg(test)]
    pub fn guarded_argv_for_identity(&self, identity: &TmuxServerIdentity) -> Vec<String> {
        guarded_tmux_argv(identity, self)
    }

    fn command_list(&self) -> String {
        self.parts
            .iter()
            .map(|part| match part {
                TmuxCommandPart::Argument(arg) => quote_tmux_argument(arg),
                TmuxCommandPart::Separator => ";".to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn render(&self, socket_path: &Path) -> String {
        format!("tmux -S {} {}", socket_path.display(), self.command_list())
    }
}

fn quote_tmux_argument(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' | '"' | '$' | ';' | '~' => {
                quoted.push('\\');
                quoted.push(character);
            }
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

fn quote_tmux_format_literal(value: &str) -> String {
    value.replace('#', "##").replace(',', "#,").replace('}', "#}")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn generation_condition(identity: &TmuxServerIdentity) -> String {
    let path = identity.socket_path.to_string_lossy();
    let format_predicate = format!(
        "#{{&&:#{{==:#{{pid}},{}}},#{{==:#{{socket_path}},{}}}}}",
        identity.server_pid,
        quote_tmux_format_literal(&path)
    );
    let mut checks = vec![format!("test x{format_predicate} = x1")];
    if let (Some(device), Some(inode)) = (identity.socket_device, identity.socket_inode) {
        checks.push(socket_file_condition(&path, device, inode));
    }
    checks.join(" && ")
}

fn socket_file_condition(path: &str, device: u64, inode: u64) -> String {
    let path = shell_quote(&quote_tmux_format_literal(path));
    format!(
        "ilmari_socket_identity=$(stat -f '%d:%i' -- {path} 2>/dev/null || stat -c '%d:%i' -- {path} 2>/dev/null) && test x\"$ilmari_socket_identity\" = x{}",
        shell_quote(&format!("{device}:{inode}"))
    )
}

fn guarded_tmux_argv(identity: &TmuxServerIdentity, command: &TmuxCommand) -> Vec<String> {
    let accepted = TmuxCommand::sequence([
        TmuxCommand::new(["display-message", "-p", GENERATION_ACCEPTED_MARKER]),
        command.clone(),
    ])
    .command_list();
    let rejected =
        TmuxCommand::new(["display-message", "-p", GENERATION_REJECTED_MARKER]).command_list();
    vec![
        "-S".to_string(),
        identity.socket_path.to_string_lossy().into_owned(),
        "if-shell".to_string(),
        generation_condition(identity),
        accepted,
        rejected,
    ]
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
    TmuxCommand::sequence([
        TmuxCommand::new(["switch-client", "-t", target.session_id.as_str()]),
        TmuxCommand::new(["select-window", "-t", target.window_id.as_str()]),
        TmuxCommand::new(["select-pane", "-t", target.pane_id.as_str()]),
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

/// Publish a popup-owned compatibility alias and revoke any daemon claim atomically.
pub fn publish_popup_alias(alias: &str, daemon_claim: &str, value: &str) -> Result<(), TmuxError> {
    run_tmux_command(&popup_alias_command(alias, daemon_claim, value))?;
    Ok(())
}

fn popup_alias_command(alias: &str, daemon_claim: &str, value: &str) -> TmuxCommand {
    TmuxCommand::sequence([
        TmuxCommand::new(["set-option", "-gu", daemon_claim]),
        TmuxCommand::new(["set-option", "-gq", alias, value]),
    ])
}

const DAEMON_LEGACY_SOCKET_CLAIM: &str = "@ilmari_daemon_legacy_socket_path";
const DAEMON_LEGACY_MCP_CLAIM: &str = "@ilmari_daemon_legacy_mcp_url";

fn claim_legacy_alias_command(
    alias: &str,
    claim: &str,
    value: &str,
    observed_stale_alias: Option<&str>,
) -> TmuxCommand {
    let mut conditions = vec![
        "#{==:#{ALIAS},}".replace("ALIAS", alias),
        "#{&&:#{!=:#{CLAIM},},#{==:#{ALIAS},#{CLAIM}}}"
            .replace("ALIAS", alias)
            .replace("CLAIM", claim),
    ];
    if let Some(observed) = observed_stale_alias {
        conditions.push(tmux_option_equals_format(alias, observed));
    }
    let condition = format!("#{{||:{}}}", conditions.join(","));
    let claim_commands = TmuxCommand::sequence([
        TmuxCommand::new(["set-option", "-gq", alias, value]),
        TmuxCommand::new(["set-option", "-gq", claim, value]),
    ]);
    TmuxCommand::new([
        "if-shell".to_string(),
        "-F".to_string(),
        condition,
        claim_commands.command_list(),
        String::new(),
    ])
}

fn clear_stale_legacy_claim_command(alias: &str, claim: &str) -> TmuxCommand {
    let clear_alias = TmuxCommand::new([
        "if-shell".to_string(),
        "-F".to_string(),
        format!("#{{==:#{{{alias}}},#{{{claim}}}}}"),
        TmuxCommand::new(["set-option", "-gu", alias]).command_list(),
        String::new(),
    ]);
    TmuxCommand::new([
        "if-shell".to_string(),
        "-F".to_string(),
        format!("#{{!=:#{{{claim}}},}}"),
        TmuxCommand::sequence([clear_alias, TmuxCommand::new(["set-option", "-gu", claim])])
            .command_list(),
        String::new(),
    ])
}

/// Publish a daemon IPC role tuple in one server-side command list.
pub fn publish_daemon_socket_path(
    path: &Path,
    owner_pid: u32,
    observed_stale_legacy: Option<&str>,
) -> Result<(), TmuxError> {
    let path = path.to_string_lossy().into_owned();
    run_tmux_command(&TmuxCommand::sequence([
        // Changing the owner first invalidates any cleanup already prepared by a
        // previous daemon before this replacement publishes the shared path.
        TmuxCommand::new([
            "set-option".to_string(),
            "-gq".to_string(),
            "@ilmari_daemon_owner_pid".to_string(),
            owner_pid.to_string(),
        ]),
        TmuxCommand::new(["set-option", "-gq", "@ilmari_daemon_socket_path", path.as_str()]),
        TmuxCommand::new(["set-option", "-gu", "@ilmari_daemon_mcp_url"]),
        clear_stale_legacy_claim_command("@ilmari_mcp_url", DAEMON_LEGACY_MCP_CLAIM),
        claim_legacy_alias_command(
            "@ilmari_socket_path",
            DAEMON_LEGACY_SOCKET_CLAIM,
            path.as_str(),
            observed_stale_legacy,
        ),
    ]))?;
    Ok(())
}

/// Publish the daemon MCP endpoint without stealing a popup-owned legacy alias.
pub fn publish_daemon_mcp_url(
    url: &str,
    owner_pid: u32,
    socket_path: &Path,
) -> Result<(), TmuxError> {
    run_tmux_command(&daemon_mcp_publication_command(url, owner_pid, socket_path))?;
    Ok(())
}

fn daemon_mcp_publication_command(url: &str, owner_pid: u32, socket_path: &Path) -> TmuxCommand {
    let role_condition = tmux_all_formats([
        tmux_option_equals_format("@ilmari_daemon_owner_pid", &owner_pid.to_string()),
        tmux_option_equals_format("@ilmari_daemon_socket_path", &socket_path.to_string_lossy()),
    ]);
    let publication = TmuxCommand::sequence([
        TmuxCommand::new(["set-option", "-gq", "@ilmari_daemon_mcp_url", url]),
        claim_legacy_alias_command("@ilmari_mcp_url", DAEMON_LEGACY_MCP_CLAIM, url, None),
    ]);
    TmuxCommand::new([
        "if-shell".to_string(),
        "-F".to_string(),
        role_condition,
        publication.command_list(),
        String::new(),
    ])
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

#[cfg(test)]
fn bind_origin_identity(value: &str, probed: TmuxServerIdentity) -> Option<TmuxServerIdentity> {
    bind_origin_identity_with_frozen_socket(value, probed, None, None)
}

fn bind_origin_identity_with_frozen_socket(
    value: &str,
    probed: TmuxServerIdentity,
    frozen_device: Option<&OsStr>,
    frozen_inode: Option<&OsStr>,
) -> Option<TmuxServerIdentity> {
    let frozen_identity = parse_frozen_socket_identity(frozen_device, frozen_inode)?;
    let (context_path, context_server_pid) = tmux_context_from_value(value)?;
    let canonical_context_path = fs::canonicalize(&context_path).unwrap_or(context_path);
    let canonical_probed_path =
        fs::canonicalize(&probed.socket_path).unwrap_or_else(|_| probed.socket_path.clone());
    let frozen_identity_matches = frozen_identity.is_none_or(|(device, inode)| {
        probed.socket_device == Some(device) && probed.socket_inode == Some(inode)
    });
    (frozen_identity_matches
        && probed.server_pid == context_server_pid
        && canonical_probed_path == canonical_context_path
        && server_identity_has_socket_file(&probed))
    .then_some(probed)
}

fn parse_frozen_socket_identity(
    frozen_device: Option<&OsStr>,
    frozen_inode: Option<&OsStr>,
) -> Option<Option<(u64, u64)>> {
    match (frozen_device, frozen_inode) {
        (None, None) => Some(None),
        (Some(device), Some(inode)) => {
            let device = device.to_str()?.parse::<u64>().ok()?;
            let inode = inode.to_str()?.parse::<u64>().ok()?;
            Some(Some((device, inode)))
        }
        _ => None,
    }
}

#[cfg(unix)]
fn server_identity_has_socket_file(identity: &TmuxServerIdentity) -> bool {
    identity.socket_device.is_some() && identity.socket_inode.is_some()
}

#[cfg(not(unix))]
fn server_identity_has_socket_file(_identity: &TmuxServerIdentity) -> bool {
    true
}

/// Resolve and freeze the tmux server generation that originated this process.
pub fn origin_server_identity() -> Option<&'static TmuxServerIdentity> {
    ORIGIN_SERVER_IDENTITY
        .get_or_init(|| {
            let context = env::var_os("TMUX")?;
            let context = context.to_string_lossy();
            let (candidate, _context_server_pid) = tmux_context_from_value(context.as_ref())?;
            let probed = probe_server_identity_at(&candidate)?;
            let frozen_device = env::var_os(TPM_ORIGIN_SOCKET_DEVICE_ENV);
            let frozen_inode = env::var_os(TPM_ORIGIN_SOCKET_INODE_ENV);
            bind_origin_identity_with_frozen_socket(
                context.as_ref(),
                probed,
                frozen_device.as_deref(),
                frozen_inode.as_deref(),
            )
        })
        .as_ref()
}

/// Return the server-reported socket path for the frozen originating tmux generation.
///
/// The original spelling is retained because tmux's receiving-server guard compares it with
/// `#{socket_path}`. Origin binding canonicalizes both paths separately so aliases such as
/// macOS `/tmp` and `/private/tmp` still identify the same server.
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
    let (socket_device, socket_inode) = socket_file_identity(&reported_path);
    Some(TmuxServerIdentity { socket_path: reported_path, server_pid, socket_device, socket_inode })
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
        .map(|value| strip_tmux_record_newline(&value).to_string())
}

fn strip_tmux_record_newline(value: &str) -> &str {
    value.strip_suffix('\n').unwrap_or(value)
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
    let output = run_tmux_command(&TmuxCommand::sequence([
        TmuxCommand::new(["list-clients", "-F", "focus:#{pane_id}"]),
        TmuxCommand::new(["display-message", "-p", "badges:#{@ilmari_badges_enabled}"]),
        TmuxCommand::new(["display-message", "-p", "status:#{@ilmari_status_enabled}"]),
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
    let panes = collect_pane_snapshots().map(|collection| collection.snapshots).unwrap_or_default();
    let mut cleanup = pane_cleanup_commands(&panes);
    for option in [
        "@ilmari_window_badges",
        "@ilmari_status_summary",
        "@ilmari_running_count",
        "@ilmari_waiting_count",
        "@ilmari_finished_count",
    ] {
        cleanup.push(TmuxCommand::new(["set-option", "-gu", option]));
    }
    if let Some(path) = owned_socket_path.as_deref() {
        cleanup.push(clear_owned_legacy_alias_command(
            "@ilmari_socket_path",
            DAEMON_LEGACY_SOCKET_CLAIM,
            path,
        ));
        cleanup.push(TmuxCommand::new(["set-option", "-gu", "@ilmari_daemon_socket_path"]));
    }
    if let Some(url) = owned_mcp_url {
        cleanup.push(clear_owned_legacy_alias_command(
            "@ilmari_mcp_url",
            DAEMON_LEGACY_MCP_CLAIM,
            url,
        ));
        cleanup.push(TmuxCommand::new(["set-option", "-gu", "@ilmari_daemon_mcp_url"]));
    }
    // Owner removal is deliberately the final destructive command.
    cleanup.push(TmuxCommand::new(["set-option", "-gu", "@ilmari_daemon_owner_pid"]));

    let role_condition = daemon_role_format_condition(
        &owned_daemon_pid,
        owned_socket_path.as_deref(),
        owned_mcp_url,
    );
    let conditional_cleanup = TmuxCommand::new([
        "if-shell".to_string(),
        "-F".to_string(),
        role_condition,
        TmuxCommand::sequence(cleanup).command_list(),
        String::new(),
    ]);
    let _ = run_tmux_command(&conditional_cleanup);
}

fn pane_cleanup_commands(panes: &[PaneSnapshot]) -> Vec<TmuxCommand> {
    let mut cleanup = Vec::with_capacity(panes.len() * 2);
    for pane in panes {
        cleanup.push(TmuxCommand::new([
            "set-option",
            "-pqu",
            "-t",
            pane.pane_id.as_str(),
            "@ilmari_state",
        ]));
        cleanup.push(TmuxCommand::new([
            "set-option",
            "-pqu",
            "-t",
            pane.pane_id.as_str(),
            "@ilmari_badge",
        ]));
    }
    cleanup
}

fn daemon_role_format_condition(
    owner_pid: &str,
    socket_path: Option<&str>,
    mcp_url: Option<&str>,
) -> String {
    let mut checks = vec![tmux_option_equals_format("@ilmari_daemon_owner_pid", owner_pid)];
    if let Some(socket_path) = socket_path {
        checks.push(tmux_option_equals_format("@ilmari_daemon_socket_path", socket_path));
    }
    // Absence is part of the role tuple: a late MCP publication invalidates a
    // cleanup prepared before MCP startup just as a different URL would.
    checks.push(tmux_option_equals_format("@ilmari_daemon_mcp_url", mcp_url.unwrap_or_default()));
    tmux_all_formats(checks)
}

fn tmux_option_equals_format(option: &str, expected: &str) -> String {
    format!("#{{==:#{{{option}}},{}}}", quote_tmux_format_literal(expected))
}

fn tmux_all_formats(checks: impl IntoIterator<Item = String>) -> String {
    format!("#{{&&:{}}}", checks.into_iter().collect::<Vec<_>>().join(","))
}

fn clear_owned_legacy_alias_command(alias: &str, claim: &str, expected: &str) -> TmuxCommand {
    let clear_alias = TmuxCommand::new([
        "if-shell".to_string(),
        "-F".to_string(),
        tmux_option_equals_format(alias, expected),
        TmuxCommand::new(["set-option", "-gu", alias]).command_list(),
        String::new(),
    ]);
    TmuxCommand::new([
        "if-shell".to_string(),
        "-F".to_string(),
        tmux_option_equals_format(claim, expected),
        TmuxCommand::sequence([clear_alias, TmuxCommand::new(["set-option", "-gu", claim])])
            .command_list(),
        String::new(),
    ])
}

#[cfg(test)]
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

#[cfg(test)]
fn publication_value_matches(current: Option<&str>, expected: &str) -> bool {
    current == Some(expected)
}

fn run_tmux_command(command: &TmuxCommand) -> Result<String, TmuxError> {
    let identity = origin_server_identity().ok_or(TmuxError::MissingSocket)?;
    if socket_file_identity(&identity.socket_path)
        != (identity.socket_device, identity.socket_inode)
    {
        return Err(TmuxError::CommandFailed {
            command: command.render(&identity.socket_path),
            exit_code: None,
            stderr: "originating tmux server socket identity changed or disappeared".to_string(),
        });
    }
    let argv = guarded_tmux_argv(identity, command);
    let output = Command::new("tmux").args(&argv).output()?;
    if !output.status.success() {
        return Err(TmuxError::CommandFailed {
            command: command.render(&identity.socket_path),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let stdout = String::from_utf8(output.stdout)?;
    decode_guarded_stdout(command, identity, output.status.code(), &stdout)
}

fn decode_guarded_stdout(
    command: &TmuxCommand,
    identity: &TmuxServerIdentity,
    exit_code: Option<i32>,
    stdout: &str,
) -> Result<String, TmuxError> {
    let Some(stdout) =
        stdout.strip_prefix(GENERATION_ACCEPTED_MARKER).and_then(|s| s.strip_prefix('\n'))
    else {
        return Err(TmuxError::CommandFailed {
            command: command.render(&identity.socket_path),
            exit_code,
            stderr: if stdout.starts_with(GENERATION_REJECTED_MARKER) {
                "originating tmux server generation changed or disappeared".to_string()
            } else {
                "tmux did not acknowledge the guarded command".to_string()
            },
        });
    };
    Ok(stdout.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        bind_origin_identity, bind_origin_identity_with_frozen_socket, capture_output_tail_command,
        capture_output_tails_with_process_kinds_using, daemon_mcp_publication_command,
        daemon_publication_matches, decode_guarded_stdout, jump_command, pane_snapshot_command,
        parse_frozen_socket_identity, parse_pane_snapshots, popup_alias_command,
        publication_value_matches, quote_tmux_argument, socket_path_from_tmux_context,
        strip_tmux_record_newline, PaneSnapshot, PaneSnapshotParseError, TmuxCommand, TmuxError,
        TmuxServerIdentity, DAEMON_LEGACY_MCP_CLAIM, DEFAULT_CAPTURE_START,
        GENERATION_ACCEPTED_MARKER, GENERATION_REJECTED_MARKER, LIST_PANES_FORMAT,
    };
    use crate::agents::SessionTracker;
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    #[test]
    fn snapshot_command_uses_global_tab_separated_format() {
        let command = pane_snapshot_command();

        assert_eq!(
            command.args(),
            vec!["list-panes".to_string(), "-aF".to_string(), LIST_PANES_FORMAT.to_string(),]
        );
    }

    #[test]
    fn replacement_between_probe_and_action_cannot_receive_an_unguarded_command() {
        let command = pane_snapshot_command();
        let identity = TmuxServerIdentity {
            socket_path: PathBuf::from("/tmp/ilmari,custom/tmux.sock"),
            server_pid: 4312,
            socket_device: Some(1),
            socket_inode: Some(99),
        };
        let argv = command.guarded_argv_for_identity(&identity);

        assert_eq!(&argv[..3], &["-S", "/tmp/ilmari,custom/tmux.sock", "if-shell"]);
        assert!(argv[3].contains("#{pid}"));
        assert!(argv[3].contains("4312"));
        assert!(argv[3].contains("#{socket_path}"));
        assert!(argv[3].contains("stat -f"));
        assert!(argv[3].contains("1:99"));
        assert!(argv[4].contains(GENERATION_ACCEPTED_MARKER));
        assert!(argv[4].contains("list-panes"));
        assert!(!argv[..4].iter().any(|argument| argument == "list-panes"));

        let rejected = format!("{GENERATION_REJECTED_MARKER}\n");
        let error = decode_guarded_stdout(&command, &identity, Some(0), &rejected)
            .expect_err("replacement response must fail closed");
        assert!(error.to_string().contains("generation changed or disappeared"));
    }

    #[test]
    fn guarded_output_strips_only_the_server_acknowledgement_line() {
        let identity = TmuxServerIdentity {
            socket_path: PathBuf::from("/tmp/tmux.sock"),
            server_pid: 10,
            socket_device: Some(1),
            socket_inode: Some(100),
        };
        let command = TmuxCommand::new(["capture-pane", "-p"]);
        let output =
            format!("{GENERATION_ACCEPTED_MARKER}\n{GENERATION_ACCEPTED_MARKER}\npane text\n");

        assert_eq!(
            decode_guarded_stdout(&command, &identity, Some(0), &output).unwrap(),
            format!("{GENERATION_ACCEPTED_MARKER}\npane text\n")
        );
    }

    #[test]
    fn same_pid_and_path_with_a_different_receiving_inode_is_rejected() {
        let origin = TmuxServerIdentity {
            socket_path: PathBuf::from("/tmp/tmux.sock"),
            server_pid: 10,
            socket_device: Some(1),
            socket_inode: Some(100),
        };
        let replacement = TmuxServerIdentity { socket_inode: Some(101), ..origin.clone() };

        let condition = super::generation_condition(&origin);
        assert!(condition.contains("#{==:#{pid},10}"));
        assert!(condition.contains("stat -f"));
        assert!(condition.contains("1:100"));
        assert!(!condition.contains("1:101"));
        assert_ne!(origin, replacement);
    }

    #[cfg(unix)]
    #[test]
    fn receiving_server_inode_predicate_rejects_same_path_replacement() {
        use std::fs;
        use std::os::unix::fs::MetadataExt;
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("ilmari-inode-{nonce}"));
        let replacement_path = std::env::temp_dir().join(format!("ilmari-inode-{nonce}.new"));
        fs::write(&path, b"origin").expect("origin entry should be created");
        let metadata = fs::metadata(&path).expect("origin entry metadata should exist");
        let condition = super::socket_file_condition(
            path.to_str().expect("temporary path should be utf-8"),
            metadata.dev(),
            metadata.ino(),
        );
        assert!(Command::new("sh").arg("-c").arg(&condition).status().unwrap().success());

        fs::write(&replacement_path, b"replacement").expect("replacement entry should be created");
        let replacement_metadata =
            fs::metadata(&replacement_path).expect("replacement entry metadata should exist");
        assert_ne!(
            (metadata.dev(), metadata.ino()),
            (replacement_metadata.dev(), replacement_metadata.ino())
        );
        fs::rename(&replacement_path, &path).expect("replacement should take over the pathname");
        assert!(!Command::new("sh").arg("-c").arg(&condition).status().unwrap().success());

        fs::remove_file(path).expect("replacement entry should unlink");
    }

    #[test]
    fn global_option_removes_only_the_one_tmux_record_newline() {
        assert_eq!(strip_tmux_record_newline("value\n"), "value");
        assert_eq!(strip_tmux_record_newline("value\n\n"), "value\n");
        assert_eq!(strip_tmux_record_newline("a\nb\n"), "a\nb");
        assert_eq!(strip_tmux_record_newline("value"), "value");
    }

    #[test]
    fn command_list_quoting_preserves_arbitrary_values_as_one_argument() {
        assert_eq!(
            quote_tmux_argument("; '$HOME' \\\n#{pane_id}\t~"),
            "\"\\; '$HOME' \\\\\\n#{pane_id}\\t\\~\"".replace("$", "\\$")
        );
        let command = TmuxCommand::sequence([
            TmuxCommand::new(["set-option", "-gq", "@value", ";"]),
            TmuxCommand::new(["display-message", "-p", "done"]),
        ]);
        assert!(command.command_list().contains("\"\\;\" ; \"display-message\""));
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
    fn frozen_tpm_socket_identity_is_optional_but_exact_when_present() {
        let responding = TmuxServerIdentity {
            socket_path: PathBuf::from("/tmp/ilmari,custom/tmux.sock"),
            server_pid: 11,
            socket_device: Some(7),
            socket_inode: Some(101),
        };
        let context = "/tmp/ilmari,custom/tmux.sock,11,7";

        assert_eq!(
            bind_origin_identity_with_frozen_socket(context, responding.clone(), None, None),
            Some(responding.clone())
        );
        assert_eq!(
            bind_origin_identity_with_frozen_socket(
                context,
                responding.clone(),
                Some(OsStr::new("7")),
                Some(OsStr::new("101")),
            ),
            Some(responding.clone())
        );
        assert!(bind_origin_identity_with_frozen_socket(
            context,
            responding.clone(),
            Some(OsStr::new("7")),
            Some(OsStr::new("102")),
        )
        .is_none());
        assert!(bind_origin_identity_with_frozen_socket(
            context,
            responding,
            Some(OsStr::new("8")),
            Some(OsStr::new("101")),
        )
        .is_none());
    }

    #[test]
    fn frozen_tpm_socket_identity_fails_closed_when_partial_or_malformed() {
        assert_eq!(parse_frozen_socket_identity(None, None), Some(None));
        assert_eq!(
            parse_frozen_socket_identity(Some(OsStr::new("7")), Some(OsStr::new("101"))),
            Some(Some((7, 101)))
        );
        assert_eq!(parse_frozen_socket_identity(Some(OsStr::new("7")), None), None);
        assert_eq!(parse_frozen_socket_identity(None, Some(OsStr::new("101"))), None);
        assert_eq!(
            parse_frozen_socket_identity(Some(OsStr::new("device")), Some(OsStr::new("101"))),
            None
        );
        assert_eq!(parse_frozen_socket_identity(Some(OsStr::new("7")), Some(OsStr::new(""))), None);
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

        let replacement_arrived_before_action = daemon_publication_matches(
            Some("43"),
            Some("/tmp/daemon.sock"),
            Some("http://127.0.0.1:4020/mcp"),
            "42",
            Some("/tmp/daemon.sock"),
            Some("http://127.0.0.1:4010/mcp"),
        );
        assert!(!replacement_arrived_before_action, "new owner render and endpoints survive");
    }

    #[test]
    fn popup_first_daemon_second_keeps_popup_legacy_mcp_alias() {
        let command = daemon_mcp_publication_command(
            "http://127.0.0.1:4010/mcp",
            42,
            std::path::Path::new("/tmp/daemon.sock"),
        );
        let list = command.command_list();

        assert!(list.contains("@ilmari_daemon_mcp_url"));
        assert!(list.contains("@ilmari_mcp_url"));
        assert!(list.contains(DAEMON_LEGACY_MCP_CLAIM));
        assert!(list.contains("@ilmari_daemon_owner_pid"));
        assert!(list.contains("@ilmari_daemon_socket_path"));
        assert!(list.contains("#{==:#{@ilmari_mcp_url},}"));
        assert!(list.contains("#{==:#{@ilmari_mcp_url},#{@ilmari_daemon_legacy_mcp_url}}"));

        let popup_alias = Some("http://127.0.0.1:4020/mcp");
        let daemon_claim: Option<&str> = None;
        let daemon_may_claim =
            popup_alias.is_none() || daemon_claim.is_some_and(|claim| popup_alias == Some(claim));
        assert!(!daemon_may_claim);
    }

    #[test]
    fn dead_ownerless_legacy_socket_is_replaced_only_if_observation_is_still_exact() {
        let stale = "/tmp/old,#{socket}.sock";
        let command = super::claim_legacy_alias_command(
            "@ilmari_socket_path",
            super::DAEMON_LEGACY_SOCKET_CLAIM,
            "/tmp/new.sock",
            Some(stale),
        );
        let list = command.command_list();

        assert!(list.contains("#{==:#{@ilmari_socket_path},/tmp/old#,##{socket#}.sock}"));
        assert!(list.contains("@ilmari_daemon_legacy_socket_path"));
        assert!(list.contains("/tmp/new.sock"));
    }

    #[test]
    fn daemon_first_claim_is_exact_and_cleanup_does_not_remove_popup_overwrite() {
        let daemon_url = "http://127.0.0.1:4010/mcp";
        let claim = daemon_url;
        let popup_overwrite = "http://127.0.0.1:4020/mcp";

        assert!(publication_value_matches(Some(claim), daemon_url));
        assert!(!publication_value_matches(Some(popup_overwrite), daemon_url));
        // Cleanup removes the exact old marker even though the now-popup-owned
        // alias is preserved, so a later daemon cannot mistake it for a claim.
        let cleanup = super::clear_owned_legacy_alias_command(
            "@ilmari_mcp_url",
            DAEMON_LEGACY_MCP_CLAIM,
            daemon_url,
        )
        .command_list();
        assert!(cleanup.contains("@ilmari_mcp_url"));
        assert!(cleanup.contains(DAEMON_LEGACY_MCP_CLAIM));
        assert!(cleanup.rfind(DAEMON_LEGACY_MCP_CLAIM) > cleanup.rfind("@ilmari_mcp_url"));
    }

    #[test]
    fn popup_republishing_identical_socket_or_mcp_revokes_stale_claim_first() {
        for (alias, claim, value) in [
            ("@ilmari_mcp_url", DAEMON_LEGACY_MCP_CLAIM, "http://127.0.0.1:4010/mcp"),
            ("@ilmari_socket_path", super::DAEMON_LEGACY_SOCKET_CLAIM, "/tmp/daemon.sock"),
        ] {
            let parts = popup_alias_command(alias, claim, value).args();
            assert_eq!(parts[0], "set-option");
            assert_eq!(parts[2], claim);
            assert_eq!(parts[3], ";");
            assert_eq!(parts[6], alias);
            assert_eq!(parts[7], value);
        }
    }

    #[test]
    fn pane_cleanup_unsets_are_quiet_when_a_scanned_pane_disappears() {
        let pane = PaneSnapshot::parse("%404\t1\t$1\ts\t@1\tw\t0\t/tmp\tsh\tgone").unwrap();
        let commands = super::pane_cleanup_commands(&[pane]);
        assert_eq!(commands.len(), 2);
        assert!(commands.iter().all(|command| command.args()[1] == "-pqu"));
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
