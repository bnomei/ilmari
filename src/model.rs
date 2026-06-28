//! Shared domain types for agent sessions, TUI view models, and published-state serialization.
//!
//! `SessionRecord` is the per-pane runtime source of truth; `AppModel` is the denormalized
//! render snapshot rebuilt on every refresh cycle.

use crate::tmux::PaneSnapshot;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Supported coding-agent identity used for detection, display, and process matching.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Codex,
    Amp,
    ClaudeCode,
    OpenCode,
    Pi,
    GeminiCli,
    AntigravityCli,
    Auggie,
    Grok,
    GitHubCopilotCli,
    CursorCli,
    Aider,
    ClineCli,
    GooseCli,
    KiroCli,
    OpenHandsCli,
}

/// Whether an `AgentKind` is active in v1 or tracked as planned work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSupport {
    Enabled,
    Planned { issue: u32 },
}

impl AgentKind {
    pub const ALL_KINDS: [Self; 16] = [
        Self::Codex,
        Self::Amp,
        Self::ClaudeCode,
        Self::OpenCode,
        Self::Pi,
        Self::GeminiCli,
        Self::AntigravityCli,
        Self::Auggie,
        Self::Grok,
        Self::GitHubCopilotCli,
        Self::CursorCli,
        Self::Aider,
        Self::ClineCli,
        Self::GooseCli,
        Self::KiroCli,
        Self::OpenHandsCli,
    ];

    pub const fn support(self) -> AgentSupport {
        match self {
            Self::Codex
            | Self::Amp
            | Self::ClaudeCode
            | Self::OpenCode
            | Self::Pi
            | Self::GeminiCli
            | Self::AntigravityCli
            | Self::Auggie
            | Self::Grok
            | Self::GitHubCopilotCli
            | Self::KiroCli => AgentSupport::Enabled,
            Self::CursorCli => AgentSupport::Planned { issue: 11 },
            Self::Aider => AgentSupport::Planned { issue: 12 },
            Self::ClineCli => AgentSupport::Planned { issue: 13 },
            Self::GooseCli => AgentSupport::Planned { issue: 14 },
            Self::OpenHandsCli => AgentSupport::Planned { issue: 16 },
        }
    }

    pub const fn is_enabled(self) -> bool {
        matches!(self.support(), AgentSupport::Enabled)
    }

    pub fn enabled_kinds() -> impl Iterator<Item = Self> {
        Self::ALL_KINDS.into_iter().filter(|kind| kind.is_enabled())
    }

    #[cfg(test)]
    pub fn planned_kinds() -> impl Iterator<Item = Self> {
        Self::ALL_KINDS.into_iter().filter(|kind| !kind.is_enabled())
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Amp => "Amp",
            Self::ClaudeCode => "Claude Code",
            Self::OpenCode => "OpenCode",
            Self::Pi => "Pi",
            Self::GeminiCli => "Gemini CLI",
            Self::AntigravityCli => "Antigravity",
            Self::Auggie => "Auggie",
            Self::Grok => "Grok",
            Self::GitHubCopilotCli => "Copilot",
            Self::CursorCli => "Cursor CLI",
            Self::Aider => "Aider",
            Self::ClineCli => "Cline CLI",
            Self::GooseCli => "Goose CLI",
            Self::KiroCli => "Kiro",
            Self::OpenHandsCli => "OpenHands CLI",
        }
    }
}

/// Classified lifecycle state for one agent pane after adapter output analysis.
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

/// One tracked agent session keyed by tmux pane id across refresh cycles.
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

/// CPU and resident memory sample for one process, in tenths-of-percent and KiB.
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

/// Rolled-up process usage for the agent binary and its descendant subtasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProcessUsage {
    pub agent: ResourceUsage,
    pub spawned: ResourceUsage,
    pub subtasks: Vec<SubtaskProcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDetailTone {
    Neutral,
    AmpDeep,
    AmpSmart,
    AmpRush,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDetail {
    pub label: String,
    pub tone: AgentDetailTone,
}

/// Workspace bucket grouping pane rows that share a derived path label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGroup {
    pub label: String,
    pub git_summary: Option<GitSummaryRow>,
    pub rows: Vec<PaneRow>,
}

/// One render-ready pane row with selection, jump-match, and visibility flags.
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

/// Cached git branch and unstaged diff stats for one repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSummaryRow {
    pub workspace_path: PathBuf,
    pub workspace_label: String,
    pub branch_name: String,
    pub insertions: u32,
    pub deletions: u32,
}

/// Denormalized TUI and IPC-facing snapshot built from current `SessionRecord` rows.
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
    use super::{AgentKind, AgentSupport, AppModel, SessionStatus};
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
        assert_eq!(AgentKind::GeminiCli.display_name(), "Gemini CLI");
        assert_eq!(AgentKind::AntigravityCli.display_name(), "Antigravity");
        assert_eq!(AgentKind::Auggie.display_name(), "Auggie");
        assert_eq!(AgentKind::Grok.display_name(), "Grok");
        assert_eq!(AgentKind::GitHubCopilotCli.display_name(), "Copilot");
        assert_eq!(AgentKind::CursorCli.display_name(), "Cursor CLI");
        assert_eq!(AgentKind::Aider.display_name(), "Aider");
        assert_eq!(AgentKind::ClineCli.display_name(), "Cline CLI");
        assert_eq!(AgentKind::GooseCli.display_name(), "Goose CLI");
        assert_eq!(AgentKind::KiroCli.display_name(), "Kiro");
        assert_eq!(AgentKind::OpenHandsCli.display_name(), "OpenHands CLI");
        assert_eq!(
            AgentKind::enabled_kinds().collect::<Vec<_>>(),
            vec![
                AgentKind::Codex,
                AgentKind::Amp,
                AgentKind::ClaudeCode,
                AgentKind::OpenCode,
                AgentKind::Pi,
                AgentKind::GeminiCli,
                AgentKind::AntigravityCli,
                AgentKind::Auggie,
                AgentKind::Grok,
                AgentKind::GitHubCopilotCli,
                AgentKind::KiroCli,
            ]
        );
    }

    #[test]
    fn planned_agent_kinds_are_issue_tracked_but_disabled() {
        let planned = [
            (AgentKind::CursorCli, 11),
            (AgentKind::Aider, 12),
            (AgentKind::ClineCli, 13),
            (AgentKind::GooseCli, 14),
            (AgentKind::OpenHandsCli, 16),
        ];

        assert_eq!(
            AgentKind::planned_kinds().collect::<Vec<_>>(),
            planned.map(|(kind, _issue)| kind).to_vec()
        );
        for (kind, issue) in planned {
            assert_eq!(kind.support(), AgentSupport::Planned { issue });
            assert!(!kind.is_enabled());
        }

        for kind in AgentKind::enabled_kinds() {
            assert_eq!(kind.support(), AgentSupport::Enabled);
            assert!(kind.is_enabled());
        }
    }

    #[test]
    fn all_agent_kinds_have_support_and_display_metadata() {
        assert_eq!(AgentKind::ALL_KINDS.len(), 16);
        for kind in AgentKind::ALL_KINDS {
            assert!(!kind.display_name().is_empty());
            match kind.support() {
                AgentSupport::Enabled => assert!(kind.is_enabled()),
                AgentSupport::Planned { issue } => {
                    assert!((10..=16).contains(&issue));
                    assert!(!kind.is_enabled());
                }
            }
        }
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
