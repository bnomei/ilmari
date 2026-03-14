use crate::tmux::PaneSnapshot;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Codex,
    Amp,
    ClaudeCode,
    OpenCode,
    Pi,
}

impl AgentKind {
    #[cfg(test)]
    pub const SUPPORTED_KINDS: [Self; 5] =
        [Self::Codex, Self::Amp, Self::ClaudeCode, Self::OpenCode, Self::Pi];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Amp => "Amp",
            Self::ClaudeCode => "Claude Code",
            Self::OpenCode => "OpenCode",
            Self::Pi => "Pi",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionStatus {
    Running,
    WaitingInput,
    Finished,
    Terminated,
    Unknown,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingInput => "waiting-input",
            Self::Finished => "finished",
            Self::Terminated => "terminated",
            Self::Unknown => "unknown",
        }
    }

    pub fn uses_retention(self) -> bool {
        matches!(self, Self::Finished)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub pane: PaneSnapshot,
    pub kind: AgentKind,
    pub status: SessionStatus,
    pub detail: Option<Arc<AgentDetail>>,
    pub output_excerpt: Option<Arc<str>>,
    pub process_usage: Option<Arc<SessionProcessUsage>>,
    pub output_fingerprint: Option<u64>,
    pub last_changed_at: Instant,
    pub last_seen_at: Instant,
    pub retained_until: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceUsage {
    pub cpu_tenths_percent: u32,
    pub memory_kib: u64,
}

impl ResourceUsage {
    pub const fn zero() -> Self {
        Self { cpu_tenths_percent: 0, memory_kib: 0 }
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            cpu_tenths_percent: self.cpu_tenths_percent.saturating_add(other.cpu_tenths_percent),
            memory_kib: self.memory_kib.saturating_add(other.memory_kib),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtaskProcess {
    pub pid: u32,
    pub depth: usize,
    pub command_label: String,
    pub usage: ResourceUsage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProcessUsage {
    pub agent: ResourceUsage,
    pub spawned: ResourceUsage,
    pub subtasks: Vec<SubtaskProcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDetailTone {
    Neutral,
    Positive,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDetail {
    pub label: String,
    pub tone: AgentDetailTone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGroup {
    pub label: String,
    pub git_summary: Option<GitSummaryRow>,
    pub rows: Vec<PaneRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    pub pane_id: String,
    pub inactive_since_label: String,
    pub output_excerpt: Option<Arc<str>>,
    pub client_label: &'static str,
    pub detail: Option<Arc<AgentDetail>>,
    pub process_usage: Option<Arc<SessionProcessUsage>>,
    pub subtasks_expanded: bool,
    pub status: SessionStatus,
    pub status_label: &'static str,
    pub is_jump_match: bool,
    pub is_selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSummaryRow {
    pub workspace_path: PathBuf,
    pub workspace_label: String,
    pub branch_name: String,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppModel {
    pub title: String,
    pub status_line: String,
    pub show_app: bool,
    pub show_git: bool,
    pub show_detail: bool,
    pub show_time: bool,
    pub show_output: bool,
    pub show_stats: bool,
    pub workspace_groups: Vec<WorkspaceGroup>,
    pub refresh_interval: Duration,
    pub last_refresh: Instant,
    pub last_refresh_wallclock: SystemTime,
}

impl AppModel {
    pub fn placeholder() -> Self {
        Self {
            title: "Agents".to_string(),
            status_line: "Waiting for tmux agent sessions.".to_string(),
            show_app: false,
            show_git: true,
            show_detail: false,
            show_time: true,
            show_output: true,
            show_stats: false,
            workspace_groups: Vec::new(),
            refresh_interval: Duration::from_secs(5),
            last_refresh: Instant::now(),
            last_refresh_wallclock: SystemTime::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentKind, AppModel, SessionStatus};
    use std::time::Duration;

    #[test]
    fn placeholder_uses_expected_defaults() {
        let model = AppModel::placeholder();

        assert_eq!(model.title, "Agents");
        assert!(!model.show_app);
        assert!(model.show_git);
        assert!(!model.show_detail);
        assert!(model.show_time);
        assert!(model.show_output);
        assert!(!model.show_stats);
        assert!(model.workspace_groups.is_empty());
        assert_eq!(model.refresh_interval, Duration::from_secs(5));
    }

    #[test]
    fn agent_display_names_are_stable() {
        assert_eq!(AgentKind::Codex.display_name(), "Codex");
        assert_eq!(AgentKind::Amp.display_name(), "Amp");
        assert_eq!(AgentKind::ClaudeCode.display_name(), "Claude Code");
        assert_eq!(AgentKind::OpenCode.display_name(), "OpenCode");
        assert_eq!(AgentKind::Pi.display_name(), "Pi");
        assert_eq!(
            AgentKind::SUPPORTED_KINDS,
            [
                AgentKind::Codex,
                AgentKind::Amp,
                AgentKind::ClaudeCode,
                AgentKind::OpenCode,
                AgentKind::Pi,
            ]
        );
    }

    #[test]
    fn retained_statuses_match_the_v1_contract() {
        assert!(SessionStatus::Finished.uses_retention());
        assert!(!SessionStatus::Terminated.uses_retention());
        assert!(!SessionStatus::Running.uses_retention());
        assert!(!SessionStatus::WaitingInput.uses_retention());
        assert!(!SessionStatus::Unknown.uses_retention());
    }
}
