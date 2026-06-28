//! Published agent state, Unix-socket IPC, and MCP resource contracts.
//!
//! Each refresh cycle materializes a versioned snapshot of visible tmux panes as JSON
//! list and detail resources. Consumers read through a line-oriented Unix socket or,
//! when enabled, subscribe to the loopback MCP server. Socket paths under the runtime
//! base are created as private `0o700` directories so co-located users cannot squat the
//! predictable `ilmari-<USER>` hierarchy.

use std::collections::{hash_map::DefaultHasher, BTreeMap, BTreeSet, HashMap};
use std::env;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

#[cfg(all(unix, feature = "socket"))]
use std::fs;
#[cfg(all(unix, feature = "socket"))]
use std::io::{BufRead, BufReader, Write};
#[cfg(all(unix, feature = "socket"))]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
#[cfg(all(unix, feature = "socket"))]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(all(unix, feature = "socket"))]
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
#[cfg(all(unix, feature = "socket"))]
use std::thread::{self, JoinHandle};

use serde::Serialize;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::model::{AgentKind, SessionRecord, SessionStatus};
use crate::tmux::{self, PaneSnapshot};

pub const SCHEMA_VERSION: u32 = 1;
#[cfg(feature = "mcp")]
pub const RESOURCE_MIME_TYPE: &str = "application/json";
pub const LIST_RESOURCE_URI: &str = "ilmari://list";

const DEFAULT_SOCKET_ENV: &str = "ILMARI_SOCKET";
const DEFAULT_SOCKET_PATH_ENV: &str = "ILMARI_SOCKET_PATH";
const SOCKET_DIR_PREFIX: &str = "ilmari";
const SOCKET_FILE_NAME: &str = "ilmari.sock";
const OVERVIEW_INSPECT_CAPTURE_START: &str = "-80";
const UNKNOWN_INSPECT_CAPTURE_START: &str = "-120";
const RESULT_CAPTURE_START: &str = "-200";
#[cfg(all(unix, feature = "socket"))]
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(all(unix, feature = "socket"))]
const SOCKET_READ_TIMEOUT: Duration = Duration::from_millis(500);

/// Opt-in Unix-socket IPC settings resolved from `ILMARI_SOCKET` and `ILMARI_SOCKET_PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcConfig {
    pub enabled: bool,
    pub socket_path: PathBuf,
}

impl IpcConfig {
    pub fn disabled() -> Self {
        Self { enabled: false, socket_path: env::temp_dir().join(SOCKET_FILE_NAME) }
    }

    pub fn from_env_map(env: &BTreeMap<String, String>) -> Self {
        let socket_path_value =
            env.get(DEFAULT_SOCKET_PATH_ENV).filter(|value| !value.trim().is_empty());
        let enabled = match env.get(DEFAULT_SOCKET_ENV).map(String::as_str) {
            Some(value) => socket_enabled_from_var(Some(value)),
            None => socket_path_value.is_some(),
        };
        let socket_path =
            socket_path_value.map(PathBuf::from).unwrap_or_else(|| default_socket_path(env));

        Self { enabled, socket_path }
    }
}

/// Inputs for materializing a `PublishedState` from the current session list.
#[derive(Debug, Clone)]
pub struct StateBuildOptions<'a> {
    pub sessions: &'a [SessionRecord],
    pub status_line: Option<&'a str>,
    pub observed_at: SystemTime,
    pub refreshed_at: Instant,
    pub refresh_interval: Duration,
    pub revision: u64,
}

/// Versioned, read-only snapshot of visible agent panes exposed to IPC and MCP consumers.
#[derive(Debug, Clone)]
pub struct PublishedState {
    #[cfg(any(feature = "socket", test))]
    producer: Producer,
    revision: u64,
    observed_at: String,
    ttl_ms: u64,
    wait_after_ms: u64,
    items: Vec<PublishedItem>,
    warnings: Vec<String>,
}

#[cfg(feature = "mcp")]
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedResource {
    pub uri: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub mime_type: &'static str,
    pub size: u32,
    pub priority: f32,
    pub last_modified: Option<String>,
}

/// Delta emitted when published resource content or the resource set changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedStateChange {
    pub changed_uris: Vec<String>,
    pub list_changed: bool,
}

/// Thread-safe handle to the latest published snapshot and its change notifications.
#[derive(Debug, Clone)]
pub struct PublishedStateHandle {
    state: Arc<RwLock<PublishedState>>,
}

impl PublishedStateHandle {
    pub fn new(initial_state: PublishedState) -> Self {
        Self { state: Arc::new(RwLock::new(initial_state)) }
    }

    #[cfg(any(feature = "socket", feature = "mcp"))]
    pub fn snapshot(&self) -> Option<PublishedState> {
        self.state.read().ok().map(|state| state.clone())
    }

    pub fn publish(&self, state: PublishedState) -> PublishedStateChange {
        let Ok(mut current) = self.state.write() else {
            return PublishedStateChange { changed_uris: Vec::new(), list_changed: false };
        };
        let previous = current.resource_content_by_uri();
        let previous_uris = previous.keys().cloned().collect::<BTreeSet<_>>();
        let next = state.resource_content_by_uri();
        let next_uris = next.keys().cloned().collect::<BTreeSet<_>>();

        let mut changed_uris =
            next.iter()
                .filter_map(|(uri, content)| {
                    if previous.get(uri) != Some(content) {
                        Some(uri.clone())
                    } else {
                        None
                    }
                })
                .collect::<BTreeSet<_>>();
        changed_uris.extend(previous_uris.difference(&next_uris).cloned());
        let list_changed = previous_uris != next_uris;

        *current = state;
        PublishedStateChange { changed_uris: changed_uris.into_iter().collect(), list_changed }
    }
}

impl PublishedState {
    pub fn empty(refresh_interval: Duration) -> Self {
        Self::from_sessions(StateBuildOptions {
            sessions: &[],
            status_line: None,
            observed_at: SystemTime::now(),
            refreshed_at: Instant::now(),
            refresh_interval,
            revision: 0,
        })
    }

    pub fn from_sessions(options: StateBuildOptions<'_>) -> Self {
        let labels = derive_workspace_labels(
            options
                .sessions
                .iter()
                .map(|session| session.pane.pane_current_path.to_string_lossy().into_owned()),
        );
        let observed_at = format_system_time(options.observed_at);
        let ttl_ms = ttl_ms(options.refresh_interval);
        let wait_after_ms = duration_ms(options.refresh_interval);
        let warnings = options
            .status_line
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| vec![line.to_string()])
            .unwrap_or_default();

        let mut items: Vec<_> = options
            .sessions
            .iter()
            .map(|session| {
                PublishedItem::from_session(
                    session,
                    &labels,
                    options.observed_at,
                    options.refreshed_at,
                    &observed_at,
                )
            })
            .collect();

        items.sort_by(|left, right| {
            consumer_state_rank(left.state)
                .cmp(&consumer_state_rank(right.state))
                .then_with(|| pane_numeric_id(&right.id).cmp(&pane_numeric_id(&left.id)))
                .then_with(|| right.id.cmp(&left.id))
        });

        Self {
            #[cfg(any(feature = "socket", test))]
            producer: Producer::current(),
            revision: options.revision,
            observed_at,
            ttl_ms,
            wait_after_ms,
            items,
            warnings,
        }
    }

    #[cfg(any(feature = "socket", test))]
    fn ping_response(&self) -> PingResponse {
        PingResponse {
            ok: true,
            response_type: "ping",
            schema_version: SCHEMA_VERSION,
            producer: self.producer.clone(),
            observed_at: self.observed_at.clone(),
        }
    }

    #[cfg(feature = "mcp")]
    pub fn resources(&self) -> Vec<PublishedResource> {
        let mut resources = Vec::with_capacity(self.items.len() + 1);
        let list_text = self.list_resource_text();
        resources.push(PublishedResource {
            uri: LIST_RESOURCE_URI.to_string(),
            name: "list".to_string(),
            title: "Ilmari agent list".to_string(),
            description: "Current read-only aggregate list of visible Ilmari agent panes. Subscribe to this resource to be notified when the aggregate list content changes.".to_string(),
            mime_type: RESOURCE_MIME_TYPE,
            size: bounded_u32(list_text.len()),
            priority: 1.0,
            last_modified: Some(self.observed_at.clone()),
        });
        resources.extend(self.items.iter().map(|item| {
            let text = self
                .detail_response_for_item(item)
                .map(|response| encode_json(&response))
                .unwrap_or_default();
            PublishedResource {
                uri: item.resource.clone(),
                name: item.resource_name(),
                title: format!("Ilmari {}", item.id),
                description: format!(
                    "Read-only detail for {} pane {} in {} state. Subscribe to this resource to be notified when this pane state, activity, output excerpt, or process summary changes.",
                    item.agent_kind.display_name(),
                    item.id,
                    item.state.as_str()
                ),
                mime_type: RESOURCE_MIME_TYPE,
                size: bounded_u32(text.len()),
                priority: item.state.resource_priority(),
                last_modified: Some(item.activity.last_changed_at.clone()),
            }
        }));
        resources
    }

    #[cfg(any(feature = "socket", feature = "mcp", test))]
    pub fn read_resource_text(&self, uri: &str) -> Result<String, ResponseError> {
        if uri == LIST_RESOURCE_URI {
            return Ok(self.list_resource_text());
        }

        if let Some(pane_id) = resource_uri_to_pane_id(uri) {
            return self.detail_resource_text(&pane_id);
        }

        Err(ResponseError { code: "not-found", message: "No resource found for that URI" })
    }

    pub fn list_resource_text(&self) -> String {
        encode_json(&self.list_response())
    }

    #[cfg(any(feature = "socket", feature = "mcp", test))]
    pub fn detail_resource_text(&self, id: &str) -> Result<String, ResponseError> {
        self.detail_response(id).map(|response| encode_json(&response))
    }

    fn resource_content_by_uri(&self) -> BTreeMap<String, String> {
        let mut resources = BTreeMap::new();
        resources.insert(LIST_RESOURCE_URI.to_string(), self.list_resource_text());
        for item in &self.items {
            if let Ok(response) = self.detail_response_for_item(item) {
                resources.insert(item.resource.clone(), encode_json(&response));
            }
        }
        resources
    }

    fn list_response(&self) -> ListResponse {
        ListResponse {
            ok: true,
            response_type: "list",
            schema_version: SCHEMA_VERSION,
            revision: self.revision,
            resource: LIST_RESOURCE_URI,
            observed_at: self.observed_at.clone(),
            ttl_ms: self.ttl_ms,
            items: self.items.iter().map(|item| item.to_list_item(self.wait_after_ms)).collect(),
            warnings: self.warnings.clone(),
        }
    }

    #[cfg(any(feature = "socket", feature = "mcp", test))]
    fn detail_response(&self, id: &str) -> Result<DetailResponse, ResponseError> {
        let normalized = normalize_requested_id(id)
            .ok_or(ResponseError { code: "invalid-id", message: "Expected detail <pane-id>" })?;
        let item = self.items.iter().find(|item| item.id == normalized).ok_or(ResponseError {
            code: "not-found",
            message: "No item found for that pane id",
        })?;

        self.detail_response_for_item(item)
    }

    fn detail_response_for_item(
        &self,
        item: &PublishedItem,
    ) -> Result<DetailResponse, ResponseError> {
        Ok(DetailResponse {
            ok: true,
            response_type: "detail",
            schema_version: SCHEMA_VERSION,
            revision: self.revision,
            resource: item.resource.clone(),
            observed_at: self.observed_at.clone(),
            ttl_ms: self.ttl_ms,
            item: item.to_detail_item(),
        })
    }
}

#[derive(Debug, Clone)]
struct PublishedItem {
    id: String,
    state: ConsumerState,
    status: SessionStatus,
    agent_kind: AgentKind,
    agent_detail: Option<String>,
    cwd: String,
    workspace_label: String,
    target: Target,
    resource: String,
    activity: Activity,
    excerpt: Option<String>,
    process: Option<ProcessSummary>,
}

impl PublishedItem {
    fn from_session(
        session: &SessionRecord,
        labels: &HashMap<String, String>,
        observed_at: SystemTime,
        refreshed_at: Instant,
        observed_at_label: &str,
    ) -> Self {
        let cwd = session.pane.pane_current_path.to_string_lossy().into_owned();
        let workspace_label = labels.get(&cwd).cloned().unwrap_or_else(|| cwd.clone());
        let last_seen_at = instant_to_system_time(session.last_seen_at, refreshed_at, observed_at);
        let last_changed_at =
            instant_to_system_time(session.last_changed_at, refreshed_at, observed_at);

        Self {
            id: session.pane.pane_id.clone(),
            state: ConsumerState::from_status(session.status),
            status: session.status,
            agent_kind: session.kind,
            agent_detail: session.detail.as_ref().map(|detail| detail.label.clone()),
            cwd,
            workspace_label,
            target: Target::from_pane(&session.pane),
            resource: resource_uri_from_pane(&session.pane),
            activity: Activity {
                last_seen_at: format_system_time(last_seen_at),
                last_changed_at: format_system_time(last_changed_at),
                inactive_ms: duration_ms(
                    refreshed_at.saturating_duration_since(session.last_changed_at),
                ),
            },
            excerpt: session.output_excerpt.as_ref().map(|excerpt| excerpt.as_ref().to_string()),
            process: session.process_usage.as_ref().map(|usage| ProcessSummary {
                agent_cpu_tenths_percent: usage.agent.cpu_tenths_percent,
                agent_memory_kib: usage.agent.memory_kib,
                spawned_cpu_tenths_percent: usage.spawned.cpu_tenths_percent,
                spawned_memory_kib: usage.spawned.memory_kib,
                subtask_count: usage.subtasks.len(),
            }),
        }
        .with_observed_at_fallback(observed_at_label)
    }

    fn with_observed_at_fallback(mut self, observed_at: &str) -> Self {
        if self.activity.last_seen_at.is_empty() {
            self.activity.last_seen_at = observed_at.to_string();
        }
        if self.activity.last_changed_at.is_empty() {
            self.activity.last_changed_at = observed_at.to_string();
        }
        self
    }

    fn to_list_item(&self, wait_after_ms: u64) -> ListItem {
        ListItem {
            resource: self.resource.clone(),
            id: self.id.clone(),
            state: self.state.as_str(),
            agent: agent_slug(self.agent_kind),
            cwd: self.cwd.clone(),
            next: next_command_for_state(self.state, &self.id, wait_after_ms),
        }
    }

    #[cfg(feature = "mcp")]
    fn resource_name(&self) -> String {
        format!("pane-{}", self.id.trim_start_matches('%'))
    }

    fn to_detail_item(&self) -> DetailItem {
        DetailItem {
            id: self.id.clone(),
            status: self.status.as_str(),
            state: self.state.as_str(),
            agent: DetailAgent {
                kind: agent_slug(self.agent_kind),
                label: self.agent_kind.display_name(),
                detail: self.agent_detail.clone(),
            },
            cwd: self.cwd.clone(),
            workspace: Workspace { path: self.cwd.clone(), label: self.workspace_label.clone() },
            target: self.target.clone(),
            activity: self.activity.clone(),
            context: Context { excerpt: self.excerpt.clone(), excerpt_redacted: false },
            process: self.process.clone(),
            commands: detail_commands(&self.target),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsumerState {
    Running,
    NeedsInput,
    Done,
    Gone,
    Unknown,
}

impl ConsumerState {
    fn from_status(status: SessionStatus) -> Self {
        match status {
            SessionStatus::Running => Self::Running,
            SessionStatus::WaitingInput => Self::NeedsInput,
            SessionStatus::Finished => Self::Done,
            SessionStatus::Terminated => Self::Gone,
            SessionStatus::Unknown => Self::Unknown,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::NeedsInput => "needs-input",
            Self::Done => "done",
            Self::Gone => "gone",
            Self::Unknown => "unknown",
        }
    }

    #[cfg(feature = "mcp")]
    const fn resource_priority(self) -> f32 {
        match self {
            Self::NeedsInput => 1.0,
            Self::Done => 0.8,
            Self::Unknown => 0.6,
            Self::Running => 0.4,
            Self::Gone => 0.2,
        }
    }
}

#[cfg(any(feature = "socket", test))]
#[derive(Debug, Clone, Serialize)]
struct Producer {
    name: &'static str,
    version: &'static str,
    pid: u32,
}

#[cfg(any(feature = "socket", test))]
impl Producer {
    fn current() -> Self {
        Self { name: "ilmari", version: env!("CARGO_PKG_VERSION"), pid: std::process::id() }
    }
}

#[cfg(any(feature = "socket", test))]
#[derive(Debug, Clone, Serialize)]
struct PingResponse {
    ok: bool,
    #[serde(rename = "type")]
    response_type: &'static str,
    schema_version: u32,
    producer: Producer,
    observed_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct ListResponse {
    ok: bool,
    #[serde(rename = "type")]
    response_type: &'static str,
    schema_version: u32,
    revision: u64,
    resource: &'static str,
    observed_at: String,
    ttl_ms: u64,
    items: Vec<ListItem>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ListItem {
    resource: String,
    id: String,
    state: &'static str,
    agent: &'static str,
    cwd: String,
    next: CommandSpec,
}

#[derive(Debug, Clone, Serialize)]
struct DetailResponse {
    ok: bool,
    #[serde(rename = "type")]
    response_type: &'static str,
    schema_version: u32,
    revision: u64,
    resource: String,
    observed_at: String,
    ttl_ms: u64,
    item: DetailItem,
}

#[derive(Debug, Clone, Serialize)]
struct DetailItem {
    id: String,
    status: &'static str,
    state: &'static str,
    agent: DetailAgent,
    cwd: String,
    workspace: Workspace,
    target: Target,
    activity: Activity,
    context: Context,
    #[serde(skip_serializing_if = "Option::is_none")]
    process: Option<ProcessSummary>,
    commands: Vec<CommandSpec>,
}

#[derive(Debug, Clone, Serialize)]
struct DetailAgent {
    kind: &'static str,
    label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Workspace {
    path: String,
    label: String,
}

#[derive(Debug, Clone, Serialize)]
struct Target {
    driver: &'static str,
    tmux_target: String,
    pane_id: String,
    pane_pid: Option<u32>,
    session_id: String,
    session_name: String,
    window_id: String,
    window_name: String,
}

impl Target {
    fn from_pane(pane: &PaneSnapshot) -> Self {
        Self {
            driver: "tmux",
            tmux_target: pane.pane_id.clone(),
            pane_id: pane.pane_id.clone(),
            pane_pid: pane.pane_pid,
            session_id: pane.session_id.clone(),
            session_name: pane.session_name.clone(),
            window_id: pane.window_id.clone(),
            window_name: pane.window_name.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Activity {
    last_seen_at: String,
    last_changed_at: String,
    inactive_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
struct Context {
    #[serde(skip_serializing_if = "Option::is_none")]
    excerpt: Option<String>,
    excerpt_redacted: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ProcessSummary {
    agent_cpu_tenths_percent: u32,
    agent_memory_kib: u64,
    spawned_cpu_tenths_percent: u32,
    spawned_memory_kib: u64,
    subtask_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct CommandSpec {
    intent: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    argv: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    argv_prefix: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    submit: Option<Vec<String>>,
}

impl CommandSpec {
    fn wait(after_ms: u64) -> Self {
        Self {
            intent: "wait",
            kind: None,
            after_ms: Some(after_ms),
            argv: None,
            argv_prefix: None,
            submit: None,
        }
    }

    fn cleanup() -> Self {
        Self {
            intent: "cleanup",
            kind: None,
            after_ms: None,
            argv: None,
            argv_prefix: None,
            submit: None,
        }
    }

    fn capture(intent: &'static str, pane_id: &str, start: &str) -> Self {
        Self {
            intent,
            kind: Some("tmux"),
            after_ms: None,
            argv: Some(vec![
                "tmux".to_string(),
                "capture-pane".to_string(),
                "-p".to_string(),
                "-J".to_string(),
                "-t".to_string(),
                pane_id.to_string(),
                "-S".to_string(),
                start.to_string(),
            ]),
            argv_prefix: None,
            submit: None,
        }
    }

    fn focus(target: &Target) -> Self {
        Self {
            intent: "focus",
            kind: Some("tmux"),
            after_ms: None,
            argv: Some(vec![
                "tmux".to_string(),
                "switch-client".to_string(),
                "-t".to_string(),
                target.session_id.clone(),
                ";".to_string(),
                "select-window".to_string(),
                "-t".to_string(),
                target.window_id.clone(),
                ";".to_string(),
                "select-pane".to_string(),
                "-t".to_string(),
                target.pane_id.clone(),
            ]),
            argv_prefix: None,
            submit: None,
        }
    }

    fn send_input(pane_id: &str) -> Self {
        Self {
            intent: "send-input",
            kind: Some("tmux"),
            after_ms: None,
            argv: None,
            argv_prefix: Some(vec![
                "tmux".to_string(),
                "send-keys".to_string(),
                "-t".to_string(),
                pane_id.to_string(),
            ]),
            submit: Some(vec!["Enter".to_string()]),
        }
    }
}

#[cfg(any(feature = "socket", test))]
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ErrorResponse {
    ok: bool,
    error: ResponseError,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResponseError {
    pub code: &'static str,
    pub message: &'static str,
}

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("socket support was not compiled in; rebuild with feature `socket`")]
    NotCompiled,
    #[error("socket already has a live listener at {0}")]
    AlreadyRunning(PathBuf),
    #[error("failed to create socket directory {path}: {source}")]
    CreateDirectory { path: PathBuf, source: io::Error },
    #[error("failed to remove stale socket {path}: {source}")]
    RemoveStaleSocket { path: PathBuf, source: io::Error },
    #[error("failed to bind socket {path}: {source}")]
    Bind { path: PathBuf, source: io::Error },
    #[error("failed to configure socket {path}: {source}")]
    Configure { path: PathBuf, source: io::Error },
    #[error("refusing to use socket directory {path}: {reason}")]
    InsecureSocketDir { path: PathBuf, reason: &'static str },
}

/// Background Unix-socket listener serving line-oriented published-state requests.
#[cfg(all(unix, feature = "socket"))]
pub struct IpcServer {
    path: PathBuf,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[cfg(all(unix, feature = "socket"))]
impl IpcServer {
    pub fn start(
        config: &IpcConfig,
        state: PublishedStateHandle,
    ) -> Result<Option<Self>, IpcError> {
        if !config.enabled {
            return Ok(None);
        }

        let listener = bind_listener(&config.socket_path)?;
        listener
            .set_nonblocking(true)
            .map_err(|source| IpcError::Configure { path: config.socket_path.clone(), source })?;

        let running = Arc::new(AtomicBool::new(true));
        let thread_state = state.clone();
        let thread_running = Arc::clone(&running);
        let handle = thread::spawn(move || serve(listener, thread_state, thread_running));

        Ok(Some(Self { path: config.socket_path.clone(), running, handle: Some(handle) }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(all(unix, feature = "socket"))]
impl Drop for IpcServer {
    fn drop(&mut self) {
        self.running.store(false, AtomicOrdering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(not(all(unix, feature = "socket")))]
pub struct IpcServer;

#[cfg(not(all(unix, feature = "socket")))]
impl IpcServer {
    pub fn start(
        config: &IpcConfig,
        _state: PublishedStateHandle,
    ) -> Result<Option<Self>, IpcError> {
        if config.enabled {
            Err(IpcError::NotCompiled)
        } else {
            Ok(None)
        }
    }

    pub fn path(&self) -> &Path {
        Path::new("")
    }
}

#[cfg(all(unix, feature = "socket"))]
fn bind_listener(path: &Path) -> Result<UnixListener, IpcError> {
    prepare_socket_directory(path)?;
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(source) if source.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(IpcError::AlreadyRunning(path.to_path_buf()));
            }
            fs::remove_file(path).map_err(|source| IpcError::RemoveStaleSocket {
                path: path.to_path_buf(),
                source,
            })?;
            UnixListener::bind(path)
                .map_err(|source| IpcError::Bind { path: path.to_path_buf(), source })
        }
        Err(source) => Err(IpcError::Bind { path: path.to_path_buf(), source }),
    }
}

#[cfg(all(unix, feature = "socket"))]
fn prepare_socket_directory(path: &Path) -> Result<(), IpcError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };

    // Every managed directory below the runtime base must be private and user-owned;
    // validating the chain outermost-first blocks co-located users from squatting the
    // predictable `ilmari-<USER>` intermediate path.
    let base = socket_base_dir();
    let mut managed: Vec<&Path> = Vec::new();
    let mut current = Some(parent);
    while let Some(dir) = current {
        if dir == base || !dir.starts_with(&base) {
            break;
        }
        managed.push(dir);
        current = dir.parent();
    }

    // An explicit `ILMARI_SOCKET_PATH` outside the runtime base is the caller's choice,
    // but a pre-existing parent still needs to be private so another local user cannot
    // squat or observe the socket.
    if managed.is_empty() {
        if parent == base {
            return Ok(());
        }
        if parent.exists() {
            ensure_private_dir(parent)?;
        } else {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(parent).map_err(|source| {
                IpcError::CreateDirectory { path: parent.to_path_buf(), source }
            })?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
                IpcError::CreateDirectory { path: parent.to_path_buf(), source }
            })?;
        }
        return Ok(());
    }

    for dir in managed.into_iter().rev() {
        ensure_private_dir(dir)?;
    }

    Ok(())
}

/// Ensure `dir` is a private directory we own, creating it `0o700` if absent.
///
/// Rejects a pre-existing entry that is a symlink, not a directory, owned by another
/// user, or grants group/other access.
#[cfg(all(unix, feature = "socket"))]
fn ensure_private_dir(dir: &Path) -> Result<(), IpcError> {
    use std::os::unix::fs::MetadataExt;

    match fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() {
                return Err(IpcError::InsecureSocketDir {
                    path: dir.to_path_buf(),
                    reason: "exists but is not a directory",
                });
            }
            if metadata.uid() != current_uid() {
                return Err(IpcError::InsecureSocketDir {
                    path: dir.to_path_buf(),
                    reason: "owned by another user",
                });
            }
            if metadata.mode() & 0o077 != 0 {
                return Err(IpcError::InsecureSocketDir {
                    path: dir.to_path_buf(),
                    reason: "grants group/other access",
                });
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .map_err(|source| IpcError::CreateDirectory { path: dir.to_path_buf(), source })?;
            // DirBuilder's mode is masked by the umask, so re-assert 0o700.
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|source| IpcError::CreateDirectory { path: dir.to_path_buf(), source })
        }
        Err(source) => Err(IpcError::CreateDirectory { path: dir.to_path_buf(), source }),
    }
}

#[cfg(all(unix, feature = "socket"))]
fn current_uid() -> u32 {
    // SAFETY: `getuid` is always successful and has no preconditions.
    unsafe { libc::getuid() }
}

#[cfg(all(unix, feature = "socket"))]
fn serve(listener: UnixListener, state: PublishedStateHandle, running: Arc<AtomicBool>) {
    while running.load(AtomicOrdering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle_stream(stream, &state),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(SOCKET_POLL_INTERVAL);
            }
            Err(_) => thread::sleep(SOCKET_POLL_INTERVAL),
        }
    }
}

#[cfg(all(unix, feature = "socket"))]
fn handle_stream(mut stream: UnixStream, state: &PublishedStateHandle) {
    let _ = stream.set_read_timeout(Some(SOCKET_READ_TIMEOUT));
    let mut request = String::new();
    let read_result = stream
        .try_clone()
        .map(BufReader::new)
        .and_then(|mut reader| reader.read_line(&mut request));

    let response = match read_result {
        Ok(0) => encode_error("empty-request", "Expected ping, list, ls, or detail <pane-id>"),
        Ok(_) => match state.snapshot() {
            Some(state) => encode_request(request.trim(), &state),
            None => encode_error("state-unavailable", "State lock is unavailable"),
        },
        Err(_) => encode_error("read-failed", "Failed to read request"),
    };

    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(b"\n");
}

#[cfg(any(feature = "socket", test))]
fn encode_request(request: &str, state: &PublishedState) -> String {
    if request == "ping" {
        return encode_json(&state.ping_response());
    }
    if matches!(request, "list" | "ls") {
        return state.list_resource_text();
    }
    if let Some(id) = request.strip_prefix("detail ") {
        return match state.detail_resource_text(id.trim()) {
            Ok(response) => response,
            Err(error) => encode_json(&ErrorResponse { ok: false, error }),
        };
    }
    if let Some(uri) = request.strip_prefix("read ") {
        return match state.read_resource_text(uri.trim()) {
            Ok(response) => response,
            Err(error) => encode_json(&ErrorResponse { ok: false, error }),
        };
    }

    encode_error("unknown-command", "Expected ping, list, ls, or detail <pane-id>")
}

#[cfg(any(feature = "socket", test))]
fn encode_error(code: &'static str, message: &'static str) -> String {
    encode_json(&ErrorResponse { ok: false, error: ResponseError { code, message } })
}

fn encode_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        "{\"ok\":false,\"error\":{\"code\":\"encode-failed\",\"message\":\"Failed to encode response\"}}".to_string()
    })
}

fn next_command_for_state(state: ConsumerState, pane_id: &str, wait_after_ms: u64) -> CommandSpec {
    match state {
        ConsumerState::Running => CommandSpec::wait(wait_after_ms),
        ConsumerState::NeedsInput => {
            CommandSpec::capture("inspect", pane_id, OVERVIEW_INSPECT_CAPTURE_START)
        }
        ConsumerState::Done => CommandSpec::capture("result", pane_id, RESULT_CAPTURE_START),
        ConsumerState::Gone => CommandSpec::cleanup(),
        ConsumerState::Unknown => {
            CommandSpec::capture("inspect", pane_id, UNKNOWN_INSPECT_CAPTURE_START)
        }
    }
}

fn detail_commands(target: &Target) -> Vec<CommandSpec> {
    vec![
        CommandSpec::focus(target),
        CommandSpec::capture("inspect", &target.pane_id, OVERVIEW_INSPECT_CAPTURE_START),
        CommandSpec::capture("result", &target.pane_id, RESULT_CAPTURE_START),
        CommandSpec::send_input(&target.pane_id),
    ]
}

fn consumer_state_rank(state: ConsumerState) -> u8 {
    match state {
        ConsumerState::NeedsInput => 0,
        ConsumerState::Done => 1,
        ConsumerState::Unknown => 2,
        ConsumerState::Running => 3,
        ConsumerState::Gone => 4,
    }
}

fn socket_enabled_from_var(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn default_socket_path(env: &BTreeMap<String, String>) -> PathBuf {
    let identity = env
        .get("TMUX")
        .and_then(|value| value.split(',').next())
        .filter(|value| !value.is_empty())
        .unwrap_or("standalone");
    let user = safe_path_segment(env.get("USER").map(String::as_str).unwrap_or("unknown"));
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    let hash = hasher.finish();

    socket_base_dir()
        .join(format!("{SOCKET_DIR_PREFIX}-{user}"))
        .join(format!("{hash:016x}"))
        .join(SOCKET_FILE_NAME)
}

fn safe_path_segment(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

fn socket_base_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn ttl_ms(refresh_interval: Duration) -> u64 {
    duration_ms(refresh_interval.saturating_mul(3))
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn instant_to_system_time(
    instant: Instant,
    reference_instant: Instant,
    reference_system_time: SystemTime,
) -> SystemTime {
    reference_system_time
        .checked_sub(reference_instant.saturating_duration_since(instant))
        .unwrap_or(reference_system_time)
}

fn format_system_time(time: SystemTime) -> String {
    OffsetDateTime::from(time).format(&Rfc3339).unwrap_or_default()
}

#[cfg(any(feature = "socket", feature = "mcp", test))]
fn normalize_requested_id(id: &str) -> Option<String> {
    let id = id.trim().strip_prefix("tmux:").unwrap_or(id.trim()).trim();
    if id.is_empty() {
        return None;
    }
    if let Some(pane_id) = resource_uri_to_pane_id(id) {
        return Some(pane_id);
    }
    if id.starts_with('%') {
        Some(id.to_string())
    } else {
        Some(format!("%{id}"))
    }
}

fn resource_uri_from_pane(pane: &PaneSnapshot) -> String {
    format!(
        "ilmari://{}/{}/{}",
        sigil_stripped_id(&pane.session_id),
        sigil_stripped_id(&pane.window_id),
        sigil_stripped_id(&pane.pane_id)
    )
}

#[cfg(any(feature = "socket", feature = "mcp", test))]
pub(crate) fn resource_uri_to_pane_id(uri: &str) -> Option<String> {
    let path = uri.trim().strip_prefix("ilmari://")?;
    if path == "list" {
        return None;
    }
    let mut parts = path.split('/');
    let _session = parts.next().filter(|part| !part.is_empty())?;
    let _window = parts.next().filter(|part| !part.is_empty())?;
    let pane = parts.next().filter(|part| !part.is_empty())?;
    if parts.next().is_some() {
        return None;
    }
    let pane = sigil_stripped_id(pane);
    if pane.is_empty() || !pane.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    Some(format!("%{pane}"))
}

fn sigil_stripped_id(id: &str) -> &str {
    id.trim_start_matches(['$', '@', '%'])
}

#[cfg(feature = "mcp")]
fn bounded_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn agent_slug(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Codex => "codex",
        AgentKind::Amp => "amp",
        AgentKind::ClaudeCode => "claude-code",
        AgentKind::OpenCode => "opencode",
        AgentKind::Pi => "pi",
        AgentKind::GeminiCli => "gemini-cli",
        AgentKind::AntigravityCli => "antigravity-cli",
        AgentKind::Auggie => "auggie",
        AgentKind::Grok => "grok",
        AgentKind::GitHubCopilotCli => "copilot",
        AgentKind::CursorCli => "cursor-cli",
        AgentKind::Aider => "aider",
        AgentKind::ClineCli => "cline-cli",
        AgentKind::GooseCli => "goose-cli",
        AgentKind::KiroCli => "kiro-cli",
        AgentKind::OpenHandsCli => "openhands-cli",
    }
}

fn pane_numeric_id(pane_id: &str) -> Option<u32> {
    pane_id.trim_start_matches('%').parse::<u32>().ok()
}

fn derive_workspace_labels(paths: impl IntoIterator<Item = String>) -> HashMap<String, String> {
    let mut unique_paths = Vec::new();
    for path in paths {
        if !unique_paths.contains(&path) {
            unique_paths.push(path);
        }
    }

    let components: Vec<Vec<String>> =
        unique_paths.iter().map(|path| path_components(path)).collect();
    let max_depth = components.iter().map(Vec::len).max().unwrap_or(1);
    let mut labels = HashMap::new();

    for depth in 1..=max_depth {
        let mut candidates: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, component_parts) in components.iter().enumerate() {
            if labels.contains_key(&unique_paths[index]) {
                continue;
            }
            candidates.entry(last_segments(component_parts, depth)).or_default().push(index);
        }

        for (label, indexes) in candidates {
            if indexes.len() == 1 {
                labels.insert(unique_paths[indexes[0]].clone(), label);
            }
        }
    }

    for path in unique_paths {
        let fallback = path.clone();
        labels.entry(path).or_insert(fallback);
    }

    labels
}

fn path_components(path: &str) -> Vec<String> {
    Path::new(path)
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(ToOwned::to_owned)
        .collect()
}

fn last_segments(components: &[String], depth: usize) -> String {
    let start = components.len().saturating_sub(depth);
    components[start..].join("/")
}

/// Publish the active socket path to `@ilmari_socket_path` for external discovery.
pub fn publish_socket_path_to_tmux(path: &Path) {
    let socket_path = path.to_string_lossy();
    let _ = tmux::set_global_option("@ilmari_socket_path", socket_path.as_ref());
}

#[cfg(test)]
mod tests {
    use super::{
        encode_request, socket_enabled_from_var, IpcConfig, PublishedState, PublishedStateHandle,
        StateBuildOptions, LIST_RESOURCE_URI,
    };
    #[cfg(all(unix, feature = "socket"))]
    use super::{
        ensure_private_dir, prepare_socket_directory, socket_base_dir, IpcError, IpcServer,
    };
    use crate::model::{
        AgentDetail, AgentDetailTone, AgentKind, ResourceUsage, SessionProcessUsage, SessionRecord,
        SessionStatus,
    };
    use crate::tmux::PaneSnapshot;
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    #[test]
    fn list_maps_statuses_to_consumer_actions() {
        let now = Instant::now();
        let state = PublishedState::from_sessions(StateBuildOptions {
            sessions: &[
                session("%11", SessionStatus::Running, now),
                session("%12", SessionStatus::WaitingInput, now),
                session("%13", SessionStatus::Finished, now),
                session("%14", SessionStatus::Terminated, now),
                session("%15", SessionStatus::Unknown, now),
            ],
            status_line: None,
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            refreshed_at: now,
            refresh_interval: Duration::from_secs(5),
            revision: 42,
        });

        let response: Value =
            serde_json::from_str(&encode_request("list", &state)).expect("valid json");
        let items = response["items"].as_array().expect("items should be an array");

        assert_eq!(response["type"], "list");
        assert_eq!(response["revision"], 42);
        assert_eq!(response["resource"], "ilmari://list");
        assert_eq!(
            items.iter().map(|item| item["state"].as_str().unwrap()).collect::<Vec<_>>(),
            vec!["needs-input", "done", "unknown", "running", "gone"]
        );
        assert_eq!(items[1]["resource"], "ilmari://1/7/13");
        assert_eq!(items[1]["next"]["intent"], "result");
        assert_eq!(
            items[1]["next"]["argv"],
            serde_json::json!(["tmux", "capture-pane", "-p", "-J", "-t", "%13", "-S", "-200"])
        );

        let alias: Value = serde_json::from_str(&encode_request("ls", &state)).expect("valid json");
        assert_eq!(alias["type"], "list");

        let removed_alias: Value =
            serde_json::from_str(&encode_request("overview", &state)).expect("valid json");
        assert_eq!(removed_alias["ok"], false);
        assert_eq!(removed_alias["error"]["code"], "unknown-command");
    }

    #[test]
    fn detail_accepts_bare_percent_and_tmux_prefixed_ids() {
        let now = Instant::now();
        let mut record = session("%12", SessionStatus::WaitingInput, now);
        record.detail = Some(Arc::new(AgentDetail {
            label: "gpt-5".to_string(),
            tone: AgentDetailTone::Neutral,
        }));
        record.output_excerpt = Some(Arc::from("Waiting for confirmation..."));
        record.process_usage = Some(Arc::new(SessionProcessUsage {
            agent: ResourceUsage { cpu_tenths_percent: 12, memory_kib: 84240 },
            spawned: ResourceUsage { cpu_tenths_percent: 0, memory_kib: 0 },
            subtasks: Vec::new(),
        }));

        let state = PublishedState::from_sessions(StateBuildOptions {
            sessions: &[record],
            status_line: Some("tail: 1 pane failed"),
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            refreshed_at: now,
            refresh_interval: Duration::from_secs(5),
            revision: 7,
        });

        for request in ["detail 12", "detail %12", "detail tmux:%12", "detail ilmari://1/7/12"] {
            let response: Value =
                serde_json::from_str(&encode_request(request, &state)).expect("valid json");

            assert_eq!(response["ok"], true);
            assert_eq!(response["revision"], 7);
            assert_eq!(response["resource"], "ilmari://1/7/12");
            assert_eq!(response["item"]["id"], "%12");
            assert_eq!(response["item"]["agent"]["kind"], "codex");
            assert_eq!(response["item"]["agent"]["detail"], "gpt-5");
            assert_eq!(response["item"]["context"]["excerpt"], "Waiting for confirmation...");
            assert_eq!(response["item"]["process"]["subtask_count"], 0);
            assert_eq!(
                response["item"]["commands"]
                    .as_array()
                    .expect("commands should be an array")
                    .iter()
                    .map(|command| command["intent"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["focus", "inspect", "result", "send-input"]
            );
        }
    }

    #[test]
    fn list_changed_tracks_resource_set_not_list_content() {
        let now = Instant::now();
        let initial = PublishedState::from_sessions(StateBuildOptions {
            sessions: &[session("%12", SessionStatus::WaitingInput, now)],
            status_line: None,
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            refreshed_at: now,
            refresh_interval: Duration::from_secs(5),
            revision: 1,
        });
        let handle = PublishedStateHandle::new(initial);

        let same_resources = PublishedState::from_sessions(StateBuildOptions {
            sessions: &[session("%12", SessionStatus::Finished, now)],
            status_line: None,
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_001),
            refreshed_at: now,
            refresh_interval: Duration::from_secs(5),
            revision: 2,
        });
        let content_change = handle.publish(same_resources);

        assert!(!content_change.list_changed);
        assert!(content_change.changed_uris.iter().any(|uri| uri == LIST_RESOURCE_URI));

        let removed_detail = PublishedState::from_sessions(StateBuildOptions {
            sessions: &[],
            status_line: None,
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_002),
            refreshed_at: now,
            refresh_interval: Duration::from_secs(5),
            revision: 3,
        });
        let resource_set_change = handle.publish(removed_detail);

        assert!(resource_set_change.list_changed);
    }

    #[test]
    fn unknown_socket_commands_return_errors() {
        let state = PublishedState::empty(Duration::from_secs(5));
        let response: Value =
            serde_json::from_str(&encode_request("wat", &state)).expect("valid json");

        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "unknown-command");
    }

    #[test]
    fn socket_config_uses_env_overrides() {
        let mut env = BTreeMap::new();
        env.insert("ILMARI_SOCKET".to_string(), "off".to_string());
        env.insert("ILMARI_SOCKET_PATH".to_string(), "/tmp/custom.sock".to_string());

        let config = IpcConfig::from_env_map(&env);

        assert!(!config.enabled);
        assert_eq!(config.socket_path, std::path::PathBuf::from("/tmp/custom.sock"));
        assert!(!socket_enabled_from_var(Some("0")));
        assert!(socket_enabled_from_var(Some("1")));
        assert!(!socket_enabled_from_var(None));

        env.remove("ILMARI_SOCKET");
        let config = IpcConfig::from_env_map(&env);
        assert!(config.enabled);
        assert_eq!(config.socket_path, std::path::PathBuf::from("/tmp/custom.sock"));
    }

    #[cfg(all(unix, feature = "socket"))]
    #[test]
    fn socket_server_answers_ping() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let socket_path = socket_base_dir().join(format!(
            "ilmari-test-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));
        let config = IpcConfig { enabled: true, socket_path: socket_path.clone() };
        let state = PublishedStateHandle::new(PublishedState::empty(Duration::from_secs(5)));
        let server = match IpcServer::start(&config, state) {
            Ok(Some(server)) => server,
            Ok(None) => panic!("socket should be enabled"),
            Err(super::IpcError::Bind { source, .. })
                if source.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                return;
            }
            Err(error) => panic!("socket should start: {error}"),
        };

        let mut stream = UnixStream::connect(&socket_path).expect("socket should accept clients");
        stream.write_all(b"ping\n").expect("request should write");

        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response should read");
        let response: Value =
            serde_json::from_str(response.trim()).expect("response should be json");

        assert_eq!(response["ok"], true);
        assert_eq!(response["type"], "ping");

        drop(server);
        assert!(!socket_path.exists());
    }

    #[cfg(all(unix, feature = "socket"))]
    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        socket_base_dir().join(format!(
            "ilmari-test-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ))
    }

    #[cfg(all(unix, feature = "socket"))]
    fn unique_outside_socket_base_dir(label: &str) -> std::path::PathBuf {
        std::env::current_dir().expect("current directory should be available").join("target").join(
            format!(
                "ilmari-test-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("system time should be after epoch")
                    .as_nanos()
            ),
        )
    }

    #[cfg(all(unix, feature = "socket"))]
    #[test]
    fn ensure_private_dir_creates_owner_only_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_temp_dir("create");
        ensure_private_dir(&dir).expect("fresh directory should be created");

        let mode = std::fs::metadata(&dir).expect("directory should exist").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);

        ensure_private_dir(&dir).expect("private directory should re-validate");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(all(unix, feature = "socket"))]
    #[test]
    fn ensure_private_dir_rejects_group_or_other_accessible_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_temp_dir("loose");
        std::fs::create_dir(&dir).expect("directory should create");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .expect("permissions should set");

        let error = ensure_private_dir(&dir).expect_err("world-accessible dir should be rejected");
        assert!(matches!(
            error,
            IpcError::InsecureSocketDir { reason: "grants group/other access", .. }
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(all(unix, feature = "socket"))]
    #[test]
    fn explicit_socket_parent_rejects_group_or_other_accessible_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_outside_socket_base_dir("explicit-loose");
        std::fs::create_dir_all(dir.parent().expect("test dir should have a parent"))
            .expect("parent should create");
        std::fs::create_dir(&dir).expect("directory should create");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .expect("permissions should set");

        let error = prepare_socket_directory(&dir.join("custom.sock"))
            .expect_err("world-accessible explicit socket parent should be rejected");
        assert!(matches!(
            error,
            IpcError::InsecureSocketDir { reason: "grants group/other access", .. }
        ));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(all(unix, feature = "socket"))]
    #[test]
    fn ensure_private_dir_rejects_symlink() {
        let target = unique_temp_dir("symlink-target");
        std::fs::create_dir(&target).expect("target should create");
        let link = unique_temp_dir("symlink-link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink should create");

        let error = ensure_private_dir(&link).expect_err("symlinked dir should be rejected");
        assert!(matches!(
            error,
            IpcError::InsecureSocketDir { reason: "exists but is not a directory", .. }
        ));

        std::fs::remove_file(&link).ok();
        std::fs::remove_dir_all(&target).ok();
    }

    fn session(pane_id: &str, status: SessionStatus, now: Instant) -> SessionRecord {
        SessionRecord {
            pane: PaneSnapshot::parse(&format!(
                "{pane_id}\t101\t$1\tdev\t@7\tagents\t0\t/workspace/ilmari\tcodex\ttitle"
            ))
            .expect("pane snapshot should parse"),
            kind: AgentKind::Codex,
            status,
            detail: None,
            output_excerpt: None,
            process_usage: None,
            output_fingerprint: None,
            last_changed_at: now,
            last_seen_at: now,
            retained_until: None,
        }
    }
}
